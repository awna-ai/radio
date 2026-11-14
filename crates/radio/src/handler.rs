use crate::{
    install,
    log::{Field, Log},
};
use radio_link::{
    EndpointId,
    handler::{Envelope, Handlers, Ran},
};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Runs only the executable recorded by an installed app.
pub struct Handler {
    clients: Option<PathBuf>,
    home: PathBuf,
    audit: Log,
    timeout: Duration,
}

impl Handler {
    pub fn new(clients: Option<PathBuf>, home: PathBuf, audit: Log, timeout: Duration) -> Self {
        Self {
            clients,
            home,
            audit,
            timeout,
        }
    }

    fn path(&self, app: &str) -> Option<PathBuf> {
        let path = install::recorded_handler(self.clients.as_ref()?, app)?;
        // SAFETY: geteuid has no pointer preconditions.
        let canonical = install::validate_handler(&path, unsafe { libc::geteuid() }).ok()?;
        (Path::new(&canonical) == path).then_some(path)
    }
}

impl Handlers for Handler {
    fn has_handler(&self, app: &str) -> bool {
        self.path(app).is_some()
    }

    fn run(&self, envelope: &Envelope<'_>) -> Ran {
        let Some(path) = self.path(envelope.app) else {
            return Ran::NoHandler;
        };
        let key = envelope.from.to_string();
        let mut command = Command::new(path);
        command
            .arg("message")
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("RADIO_FROM_KEY", &key)
            .env("RADIO_FROM_NAME", envelope.from_name.unwrap_or(&key))
            .env("RADIO_MESSAGE_ID", envelope.id)
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let Ok(mut child) = command.spawn() else {
            return Ran::NoHandler;
        };
        let start = Instant::now();
        let mut input = child.stdin.take().expect("piped stdin");
        let mut output = child.stdout.take().expect("piped stdout");
        if !nonblocking(input.as_raw_fd()) || !nonblocking(output.as_raw_fd()) {
            kill_and_reap(&mut child);
            return Ran::BadReply;
        }
        let body = envelope.body.to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let deadline = self.timeout;
        let writer = thread::Builder::new().spawn(move || {
            let mut bytes = body.as_bytes();
            while !bytes.is_empty()
                && !cancelled.load(Ordering::Relaxed)
                && start.elapsed() < deadline
            {
                match input.write(bytes) {
                    Ok(0) => break,
                    Ok(n) => bytes = &bytes[n..],
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        let Ok(writer) = writer else {
            kill_and_reap(&mut child);
            return Ran::BadReply;
        };
        let mut status = None;
        let mut bytes = Vec::new();
        let mut ended = false;
        let result = loop {
            if !ended {
                let mut buffer = [0; 4097];
                match output.read(&mut buffer[..4097 - bytes.len()]) {
                    Ok(0) => ended = true,
                    Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        kill_and_reap(&mut child);
                        break Ran::BadReply;
                    }
                }
                if bytes.len() > 4096 {
                    kill_and_reap(&mut child);
                    break Ran::BadReply;
                }
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(value) => status = value,
                    Err(_) => {
                        kill_and_reap(&mut child);
                        break Ran::BadReply;
                    }
                }
            }
            if start.elapsed() >= self.timeout {
                kill_and_reap(&mut child);
                break Ran::Timeout;
            }
            if let Some(status) = &status
                && ended
            {
                let exit = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                break match String::from_utf8(bytes) {
                    Ok(reply) if valid_reply(&reply) => Ran::Reply { reply, exit },
                    _ => Ran::BadReply,
                };
            }
            thread::sleep(
                Duration::from_millis(5).min(self.timeout.saturating_sub(start.elapsed())),
            );
        };
        cancel.store(true, Ordering::Relaxed);
        let _ = writer.join();
        result
    }

    fn refused(&self, from: &EndpointId, from_name: Option<&str>, app: &str, reason: &str) {
        let key = from.to_string();
        self.audit.event(
            "refused",
            &key,
            &[
                ("name", Field::Text(from_name.unwrap_or(&key))),
                ("app", Field::Text(app)),
                ("reason", Field::Text(reason)),
            ],
        );
    }
}

fn nonblocking(fd: RawFd) -> bool {
    // SAFETY: fd belongs to a live pipe and both fcntl commands take integers.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags != -1 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    }
}

fn kill_and_reap(child: &mut Child) {
    // SAFETY: the live child's id is its process-group id, set before exec above.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.wait();
}

fn valid_reply(reply: &str) -> bool {
    reply.lines().count() <= 8
        && reply
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control())
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
