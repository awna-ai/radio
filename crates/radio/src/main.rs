use radio::install::{
    self, InstallArgs, Installed, Installer, Launchctl, Paths, RoomsOutcome, SocketProbe, Timing,
    Uninstalled,
};
use radio::log::{Field, Log};
use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    sync::{TryLockError, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Stay below the installer’s roughly 4.5 s bootout wait even with a stalled client.
const STOP_WAIT: Duration = Duration::from_secs(2);

const USAGE: &str = "usage: radio serve [--name <name>]\n       radio install --app <name> --app-version <version> --handler <path> [--rooms <path>]\n       radio uninstall --app <name>\n       radio status\n       radio hello\n       radio id\n       radio fleet init <name>\n       radio code create [--name <name>] [--ttl <n>m|<n>h]\n       radio code list\n       radio code revoke <id>\n       radio computer list\n       radio computer accept <computer>\n       radio computer block <computer>\n       radio computer trust <computer>\n       radio register --app <app> --app-version <version> [--member <kind>:<name>]...\n       radio room create <room> --title <title>\n       radio rooms [--after <room>] [--limit <n>]\n       radio post <room> --as <kind>:<name> (--body <text> | --body-file <path|->)\n       radio read <room> [--since <seq>] [--limit <n>]\n       radio line list <room>\n       radio line set <room> <key> <line> --as <kind>:<name> [--expires-at <seconds>]\n       radio line clear <room> <key> --as <kind>:<name>";

fn host_name(explicit: Option<&str>, detected: &str) -> Result<String, ()> {
    if let Some(name) = explicit {
        return if !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            Ok(name.into())
        } else {
            Err(())
        };
    }
    let name: String = detected
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(64)
        .collect();
    Ok(if name.is_empty() {
        "local".into()
    } else {
        name
    })
}

fn detected_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: buffer is writable for the provided length; a trailing NUL is checked below.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return String::new();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

enum Cli {
    Serve(Option<String>),
    Install(InstallArgs),
    Uninstall(String),
    Status,
    Operation(radio::cli::Command),
}

fn parse(args: &[String]) -> Result<Cli, ()> {
    match args {
        [serve] if serve == "serve" => Ok(Cli::Serve(None)),
        [serve, flag, name] if serve == "serve" && flag == "--name" => {
            Ok(Cli::Serve(Some(name.clone())))
        }
        [status] if status == "status" => Ok(Cli::Status),
        [uninstall, flag, app]
            if uninstall == "uninstall" && flag == "--app" && !app.starts_with("--") =>
        {
            Ok(Cli::Uninstall(app.clone()))
        }
        [install, flags @ ..] if install == "install" => {
            let (mut app, mut app_version, mut handler, mut rooms) = (None, None, None, None);
            let (pairs, remainder) = flags.as_chunks::<2>();
            for pair in pairs {
                let slot = match pair[0].as_str() {
                    "--app" => &mut app,
                    "--app-version" => &mut app_version,
                    "--handler" => &mut handler,
                    "--rooms" => &mut rooms,
                    _ => return Err(()),
                };
                if slot.is_some() || pair[1].starts_with("--") {
                    return Err(());
                }
                *slot = Some(pair[1].clone());
            }
            if !remainder.is_empty() {
                return Err(());
            }
            Ok(Cli::Install(InstallArgs {
                app: app.ok_or(())?,
                app_version: app_version.ok_or(())?,
                handler: handler.ok_or(())?.into(),
                rooms: rooms.map(PathBuf::from),
            }))
        }
        _ => radio::cli::parse(args).map(Cli::Operation),
    }
}

fn print_install(outcome: install::InstallOutcome, app: &str) -> Result<(), (i32, String)> {
    if outcome.record_unreadable {
        println!("the install record could not be read, so radio was installed again");
    }
    if outcome.binary_changed {
        println!("the installed radio binary was missing or changed, so radio was installed again");
    }
    match outcome.installed {
        Installed::Fresh { version } => println!("radio {version} installed for {app}"),
        Installed::Upgraded { from, to } => {
            println!("radio upgraded from {from} to {to} for {app}");
        }
        Installed::Kept { installed } => {
            println!("radio {installed} is already installed; {app} is now a client");
        }
    }
    match outcome.rooms {
        Some(RoomsOutcome::Imported(report)) => println!(
            "rooms imported: {} added, {} merged, {} messages, {} status lines, {} skipped",
            report.rooms_added,
            report.rooms_merged,
            report.messages,
            report.statuses,
            report.skipped
        ),
        Some(RoomsOutcome::AlreadyImported) => println!("rooms: that store was imported before"),
        Some(RoomsOutcome::Failed(phrase)) => {
            return Err((
                4,
                format!("radio: installed, but the rooms were not imported: {phrase}"),
            ));
        }
        None => {}
    }
    Ok(())
}

fn home() -> Result<PathBuf, (i32, String)> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| (1, "radio: HOME is not set".into()))
}

fn stop_signals() -> Result<libc::sigset_t, &'static str> {
    let mut signals = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes the writable signal set before any use.
    if unsafe { libc::sigemptyset(signals.as_mut_ptr()) } != 0 {
        return Err("stop signals could not be set up");
    }
    // SAFETY: sigemptyset succeeded, so the signal set is initialized.
    let mut signals = unsafe { signals.assume_init() };
    // SAFETY: signals is initialized and writable; both signal numbers are valid.
    if unsafe {
        libc::sigaddset(&mut signals, libc::SIGTERM) != 0
            || libc::sigaddset(&mut signals, libc::SIGINT) != 0
            || libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) != 0
    } {
        return Err("stop signals could not be set up");
    }
    Ok(signals)
}

fn run(args: &[String]) -> Result<(), (i32, String)> {
    let command = parse(args).map_err(|()| (2, USAGE.into()))?;
    if let Cli::Operation(operation) = command {
        let request = operation.resolve(io::stdin().lock())?;
        let home = home()?;
        let (response, line) =
            radio_client::request_line(&radio_client::socket_path(&home), &request)
                .map_err(|phrase| (1, format!("radio: {phrase}")))?;
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            std::process::exit(1);
        }
        std::process::exit(if response.ok {
            0
        } else if response.refused == Some(true) {
            3
        } else {
            1
        });
    }
    let host = if let Cli::Serve(explicit) = &command {
        Some(host_name(explicit.as_deref(), &detected_name()).map_err(|()| (2, USAGE.into()))?)
    } else {
        None
    };
    let home = home()?;
    if let Some(host) = host {
        let paths = Paths::new(&home).unwrap_or_else(|error| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_secs());
            let _ = io::stderr().write_all(
                radio::log::line(now, "serve", "stopped", &[("error", Field::Text(&error))])
                    .as_bytes(),
            );
            std::process::exit(1);
        });
        let log = Log::stderr(&paths.log);
        let protocol = format!(
            "{}.{}",
            radio_client::PROTOCOL_MAJOR,
            radio_client::PROTOCOL_MINOR
        );
        log.event(
            "serve",
            "start",
            &[
                ("version", Field::Text(env!("CARGO_PKG_VERSION"))),
                ("protocol", Field::Text(&protocol)),
                ("name", Field::Text(&host)),
            ],
        );
        let signals = stop_signals().unwrap_or_else(|error| {
            log.event("serve", "stopped", &[("error", Field::Text(error))]);
            std::process::exit(1);
        });
        let socket = radio_client::socket_path(&home);
        let db = home.join(".local/state/radio/rooms.db");
        let server = radio::server::Server::bind(&socket, &db, host).unwrap_or_else(|error| {
            log.event("serve", "stopped", &[("error", Field::Text(&error))]);
            std::process::exit(1);
        });
        log.event("serve", "bound", &[("socket", Field::Text("radio.sock"))]);
        let busy = server.busy();
        let stopping = server.stopping();
        let stop_log = log.clone();
        std::thread::spawn(move || {
            loop {
                let mut signal = 0;
                // SAFETY: signals is initialized; signal points to writable storage.
                if unsafe { libc::sigwait(&signals, &mut signal) } == 0 {
                    stopping.store(true, Ordering::SeqCst);
                    let deadline = Instant::now() + STOP_WAIT;
                    let _guard = loop {
                        match busy.try_lock() {
                            Ok(guard) => break Some(guard),
                            Err(TryLockError::Poisoned(poison)) => break Some(poison.into_inner()),
                            Err(TryLockError::WouldBlock) => {}
                        }
                        if Instant::now() >= deadline {
                            break None;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    };
                    stop_log.event(
                        "serve",
                        "stop",
                        &[(
                            "signal",
                            Field::Text(if signal == libc::SIGTERM {
                                "TERM"
                            } else {
                                "INT"
                            }),
                        )],
                    );
                    std::process::exit(0);
                }
            }
        });
        server
            .with_link_dir(home.join(".config/radio"))
            .with_fleet_db(home.join(".local/state/radio/fleet.db"))
            .with_log(log)
            .serve();
        return Ok(());
    }
    let failed = |message| (1, format!("radio: {message}"));
    let paths = Paths::new(&home).map_err(failed)?;
    if matches!(command, Cli::Status) {
        let status = install::status(&paths, &SocketProbe);
        print!("{}", install::render(&status));
        std::process::exit(if status.ok { 0 } else { 1 });
    }
    let launchd = Launchctl::resolve(&home, env::var_os("RADIO_LAUNCHCTL")).map_err(failed)?;
    let source = env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| failed("this binary's path could not be read".into()))?;
    let installer = Installer {
        paths,
        launchd: &launchd,
        probe: &SocketProbe,
        timing: Timing::REAL,
        version: env!("CARGO_PKG_VERSION").into(),
        source,
        // SAFETY: geteuid has no arguments or memory preconditions.
        uid: unsafe { libc::geteuid() },
    };
    let failure = |error: install::Failure| (error.code, format!("radio: {}", error.message));
    match command {
        Cli::Install(args) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| failed("the current time could not be read".into()))?
                .as_secs();
            print_install(installer.install(&args, now).map_err(failure)?, &args.app)
        }
        Cli::Uninstall(app) => {
            match installer.uninstall(&app).map_err(failure)? {
                Uninstalled::NotAClient => println!("{app} was not a client"),
                Uninstalled::Removed { last: false } => {
                    println!("{app} removed; radio stays for the other clients");
                }
                Uninstalled::Removed { last: true } => println!(
                    "{app} removed; it was the last client, so radio was removed (rooms and other data are kept in ~/.local/state/radio)"
                ),
            }
            Ok(())
        }
        Cli::Serve(_) | Cli::Status | Cli::Operation(_) => unreachable!(),
    }
}

fn main() {
    let args = env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().map_err(|_| (2, USAGE.into())))
        .collect::<Result<Vec<_>, _>>();
    if let Err((code, message)) = args.and_then(|args| run(&args)) {
        let _ = writeln!(io::stderr(), "{message}");
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_rules() {
        assert_eq!(
            host_name(None, "Garden-Shed.local").ok().as_deref(),
            Some("garden-shed")
        );
        assert_eq!(host_name(None, "..").ok().as_deref(), Some("local"));
        assert_eq!(host_name(None, "_A!7-é.local").ok().as_deref(), Some("a7-"));
        assert_eq!(host_name(None, &"A".repeat(80)).unwrap(), "a".repeat(64));
        for valid in ["Garden-Shed", "a._-0", &"a".repeat(64)] {
            assert_eq!(host_name(Some(valid), "ignored").unwrap(), valid);
        }
        for invalid in ["", ".bad/path", "a b", "é", &"a".repeat(65)] {
            assert!(host_name(Some(invalid), "ignored").is_err());
        }
    }
}
