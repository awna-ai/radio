use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let mut random = [0u8; 16];
        File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut random)
            .unwrap();
        let dir =
            PathBuf::from("/tmp").join(format!("rd-log-{:032x}", u128::from_ne_bytes(random)));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        Self(dir)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
        command.args(args).env("HOME", &self.0);
        command
    }
    fn lines(&self) -> Vec<String> {
        without_time(&fs::read_to_string(self.0.join("events")).unwrap())
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn without_time(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_once(' ').unwrap().1.to_owned())
        .collect()
}

#[test]
fn daemon_logs_start_bound_and_term() {
    let home = Home::new();
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(home.0.join("events"))
        .unwrap();
    let mut daemon = Daemon(
        home.command(&["serve", "--name", "test-host"])
            .stdout(Stdio::null())
            .stderr(file)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if home.command(&["hello"]).output().unwrap().status.success() {
            break;
        }
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited before hello: {:?}",
            home.lines()
        );
        assert!(Instant::now() < deadline, "hello timed out");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        home.lines(),
        vec![
            format!(
                "serve start version=\"{}\" protocol=\"1.{}\" name=\"test-host\"",
                env!("CARGO_PKG_VERSION"),
                radio_client::PROTOCOL_MINOR
            ),
            "serve bound socket=\"radio.sock\"".into(),
        ]
    );

    let second = home
        .command(&["serve", "--name", "test-host"])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(
        without_time(std::str::from_utf8(&second.stderr).unwrap()),
        vec![
            format!(
                "serve start version=\"{}\" protocol=\"1.{}\" name=\"test-host\"",
                env!("CARGO_PKG_VERSION"),
                radio_client::PROTOCOL_MINOR
            ),
            "serve stopped error=\"another Radio serves radio.sock\"".into(),
        ]
    );

    // SAFETY: the live child pid is representable as pid_t; kill has no memory preconditions.
    assert_eq!(
        unsafe { libc::kill(daemon.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "SIGTERM did not stop the daemon");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(home.lines().last().unwrap(), "serve stop signal=\"TERM\"");
}
