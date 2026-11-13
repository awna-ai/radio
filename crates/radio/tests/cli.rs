use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::{FromRawFd, OwnedFd},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[path = "support/contract_match.rs"]
mod contract_match;

const USAGE: &str = "usage: radio serve [--name <name>]\n       radio install --app <name> --app-version <version> --handler <path> [--rooms <path>]\n       radio uninstall --app <name>\n       radio status\n       radio hello\n       radio register --app <app> --app-version <version> [--member <kind>:<name>]...\n       radio room create <room> --title <title>\n       radio rooms [--after <room>] [--limit <n>]\n       radio post <room> --as <kind>:<name> (--body <text> | --body-file <path|->)\n       radio read <room> [--since <seq>] [--limit <n>]\n       radio line list <room>\n       radio line set <room> <key> <line> --as <kind>:<name> [--expires-at <seconds>]\n       radio line clear <room> <key> --as <kind>:<name>\n";

#[test]
fn invalid_arguments_and_names_print_only_usage() {
    for args in [
        vec![],
        vec!["serve", "--name"],
        vec!["serve", "--other", "garden"],
        vec!["serve", "--name", "bad/name"],
        vec!["serve", "--name", ""],
        vec!["serve", "--name", "garden", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_radio"))
            .args(args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, USAGE.as_bytes());
    }
}

#[test]
fn non_utf8_arguments_print_usage_and_send_nothing() {
    let server = StandIn::start(r#"{"ok":true}"#);
    let invalid = OsStr::from_bytes(&[0xff]);
    for args in [
        vec![
            OsStr::new("post"),
            OsStr::new("garden"),
            OsStr::new("--as"),
            OsStr::new("helper:a"),
            OsStr::new("--body"),
            invalid,
        ],
        vec![invalid],
    ] {
        let output = server.home.command(&[]).args(args).output().unwrap();
        assert_usage(output);
        server.assert_no_connection();
    }
}

#[test]
fn stderr_write_errors_preserve_the_usage_exit_code() {
    let mut descriptors = [-1; 2];
    // SAFETY: descriptors has room for both file descriptors returned by pipe.
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    // SAFETY: pipe succeeded and each descriptor is owned exactly once here.
    let (reader, writer) = unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    };
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_radio"))
        .arg("unknown")
        .stderr(Stdio::from(writer))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_or_empty_home_has_a_fixed_error() {
    for args in [
        vec!["serve", "--name", "test-host"],
        vec![
            "install",
            "--app",
            "gardenapp",
            "--app-version",
            "2.1.0",
            "--handler",
            "/unused",
        ],
        vec!["uninstall", "--app", "gardenapp"],
        vec!["status"],
        vec!["hello"],
        vec!["register", "--app", "lantern", "--app-version", "1.0.0"],
        vec!["room", "create", "garden", "--title", "Garden"],
        vec!["rooms"],
        vec![
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body",
            "Hello garden",
        ],
        vec!["read", "garden"],
        vec!["line", "list", "garden"],
        vec![
            "line",
            "set",
            "garden",
            "mood",
            "Ready",
            "--as",
            "helper:juniper",
        ],
        vec!["line", "clear", "garden", "mood", "--as", "helper:juniper"],
    ] {
        for empty in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
            command.args(&args);
            if empty {
                command.env("HOME", "");
            } else {
                command.env_remove("HOME");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, b"radio: HOME is not set\n");
        }
    }
}

#[test]
fn install_accepts_each_flag_order_before_checking_home() {
    let flags = [
        ["--app", "gardenapp"],
        ["--app-version", "2.1.0"],
        ["--handler", "/unused"],
        ["--rooms", "/old/rooms.db"],
    ];
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).any(|i| order[i + 1..].contains(&order[i])) {
                        continue;
                    }
                    let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
                    command.arg("install").env_remove("HOME");
                    for index in order {
                        command.args(flags[index]);
                    }
                    let output = command.output().unwrap();
                    assert_eq!(output.status.code(), Some(1));
                    assert!(output.stdout.is_empty());
                    assert_eq!(output.stderr, b"radio: HOME is not set\n");
                }
            }
        }
    }
}

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let home = Self(PathBuf::from(format!(
            "/tmp/rd-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        fs::create_dir(&home.0).unwrap();
        home
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
        command.args(args).env("HOME", &self.0);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn input(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(input);
        child.wait_with_output().unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Daemon {
    home: Home,
    child: Child,
}

impl Daemon {
    fn start() -> Self {
        let home = Home::new();
        let child = home
            .command(&["serve", "--name", "test-host"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut daemon = Self { home, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if daemon.home.run(&["hello"]).status.success() {
                return daemon;
            }
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited before hello"
            );
            assert!(
                Instant::now() < deadline,
                "radio hello did not succeed within 10 seconds"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct StandIn {
    home: Home,
    requests: mpsc::Receiver<Value>,
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl StandIn {
    fn start(answer: &str) -> Self {
        let home = Home::new();
        let socket = radio_client::socket_path(&home.0);
        let folder = socket.parent().unwrap();
        fs::create_dir_all(folder).unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (sent, requests) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let answer = format!("{answer}\n");
        let worker = thread::spawn(move || {
            loop {
                if stopped.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(&stream).read_line(&mut line).unwrap();
                        sent.send(serde_json::from_str(&line).unwrap()).unwrap();
                        stream.write_all(answer.as_bytes()).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
        });
        Self {
            home,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn request(&self) -> Value {
        self.requests
            .recv_timeout(Duration::from_secs(2))
            .expect("CLI sent no request")
    }

    fn assert_no_connection(&self) {
        assert!(
            matches!(
                self.requests.recv_timeout(Duration::from_millis(30)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "unexpected connection"
        );
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

#[test]
fn stand_in_waits_for_a_slow_request() {
    use std::os::unix::net::UnixStream;
    let server = StandIn::start(r#"{"ok":true}"#);
    let mut stream = UnixStream::connect(radio_client::socket_path(&server.home.0)).unwrap();
    thread::sleep(Duration::from_millis(100));
    stream.write_all(b"{\"v\":1,\"op\":\"hello\"}\n").unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    assert_eq!(answer, "{\"ok\":true}\n");
    assert_eq!(server.request(), serde_json::json!({"v": 1, "op": "hello"}));
}

#[test]
fn stand_in_drop_handles_a_worker_panic_during_unwind() {
    fn failed_worker() -> StandIn {
        let (_, requests) = mpsc::channel();
        let (stop, _) = mpsc::channel();
        StandIn {
            home: Home::new(),
            requests,
            stop,
            worker: Some(thread::spawn(|| panic!("worker failed"))),
        }
    }
    if std::env::var_os("RADIO_TEST_UNWIND").is_some() {
        assert!(
            std::panic::catch_unwind(|| {
                let _server = failed_worker();
                panic!("outer failure");
            })
            .is_err()
        );
        return;
    }
    assert!(std::panic::catch_unwind(|| drop(failed_worker())).is_err());
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stand_in_drop_handles_a_worker_panic_during_unwind",
            "--test-threads=2",
        ])
        .env("RADIO_TEST_UNWIND", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn fixture_commands() -> Vec<(usize, Vec<&'static str>)> {
    vec![
        (1, vec!["hello"]),
        (
            2,
            vec![
                "register",
                "--app",
                "lantern",
                "--app-version",
                "1.0.0",
                "--member",
                "helper:juniper",
            ],
        ),
        (3, vec!["room", "create", "garden", "--title", "Garden"]),
        (
            4,
            vec![
                "post",
                "garden",
                "--as",
                "helper:juniper",
                "--body",
                "Hello garden",
            ],
        ),
        (
            5,
            vec![
                "line",
                "set",
                "garden",
                "mood",
                "Ready",
                "--as",
                "helper:juniper",
            ],
        ),
        (
            6,
            vec!["line", "clear", "garden", "mood", "--as", "helper:juniper"],
        ),
        (
            7,
            vec![
                "line",
                "set",
                "garden",
                "plan",
                "Plant seeds",
                "--as",
                "human:sam",
                "--expires-at",
                "4102444800",
            ],
        ),
        (8, vec!["rooms"]),
        (9, vec!["read", "garden"]),
        (10, vec!["line", "list", "garden"]),
        (
            12,
            vec![
                "post",
                "garden",
                "--as",
                "scribe:wren",
                "--body",
                "Hello garden",
            ],
        ),
        (13, vec!["read", "garden", "--since", "2", "--limit", "1"]),
        (14, vec!["rooms", "--limit", "1"]),
        (15, vec!["rooms", "--after", "garden"]),
        (16, vec!["room", "create", "garden", "--title", "Changed"]),
    ]
}

fn fixture(number: usize) -> Value {
    let folder = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/v1"));
    let prefix = format!("{number:02}-");
    let path = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&prefix)
        })
        .unwrap();
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn assert_usage(output: Output) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, USAGE.as_bytes());
}

#[test]
fn commands_send_the_exact_contract_requests() {
    let server = StandIn::start(r#"{"ok":true}"#);
    for (number, args) in fixture_commands() {
        let output = server.home.run(&args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert_eq!(server.request(), fixture(number)["request"], "{args:?}");
    }
    let args = [
        "post",
        "garden",
        "--as",
        "helper:juniper",
        "--body-file",
        "-",
    ];
    assert!(server.home.input(&args, b"Hello garden").status.success());
    assert_eq!(server.request(), fixture(4)["request"]);
    let file = server.home.0.join("body");
    fs::write(&file, "Hello garden").unwrap();
    let args = [
        "post",
        "garden",
        "--as",
        "helper:juniper",
        "--body-file",
        file.to_str().unwrap(),
    ];
    assert!(server.home.run(&args).status.success());
    assert_eq!(server.request(), fixture(4)["request"]);
}

#[test]
fn commands_replay_the_contract_against_a_real_daemon() {
    let daemon = Daemon::start();
    for (number, args) in fixture_commands() {
        let recorded = fixture(number);
        let output = daemon.home.run(&args);
        let code = match number {
            12 => 3,
            16 => 1,
            _ => 0,
        };
        assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
        if recorded["exact"] == true {
            assert_eq!(recorded["response"], actual, "{args:?}");
        } else {
            assert!(
                contract_match::contained(&recorded["response"], &actual, ""),
                "{args:?}: {} not in {actual}",
                recorded["response"]
            );
        }
    }
}

#[test]
fn daemon_answer_bytes_and_exit_codes_are_preserved() {
    for (answer, code) in [
        (r#"{"ok":true,  "later":{"b":1,"a":2}}"#, 0),
        (r#"{"ok":true,"refused":true}"#, 0),
        (r#"{"ok":false,"error":"x","refused":true}"#, 3),
        (r#"{"ok":false,"error":"x"}"#, 1),
    ] {
        let server = StandIn::start(answer);
        let output = server.home.run(&["hello"]);
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(output.stdout, format!("{answer}\n").as_bytes());
        assert!(output.stderr.is_empty());
        server.request();
    }
}

#[test]
fn version_flags_send_nothing() {
    let server = StandIn::start(r#"{"ok":true}"#);
    for (_, mut args) in fixture_commands() {
        args.extend(["--v", "2"]);
        assert_usage(server.home.run(&args));
    }
    server.assert_no_connection();
}

#[test]
fn malformed_members_send_nothing() {
    let server = StandIn::start(r#"{"ok":true}"#);
    for member in ["helper", "Helper:ada", ":ada", "helper:"] {
        for args in [
            vec![
                "register",
                "--app",
                "lantern",
                "--app-version",
                "1.0.0",
                "--member",
                member,
            ],
            vec!["post", "garden", "--as", member, "--body", "Hello garden"],
            vec!["line", "set", "garden", "mood", "Ready", "--as", member],
            vec!["line", "clear", "garden", "mood", "--as", member],
        ] {
            assert_usage(server.home.run(&args));
        }
    }
    server.assert_no_connection();
}

#[test]
fn commands_without_a_daemon_have_only_the_transport_error() {
    let home = Home::new();
    for (_, args) in fixture_commands() {
        let output = home.run(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            format!("radio: {}\n", radio_client::NOT_RUNNING).as_bytes()
        );
    }
}

#[test]
fn daemon_checks_the_message_limit_for_each_body_source() {
    let daemon = Daemon::start();
    assert!(
        daemon
            .home
            .run(&[
                "register",
                "--app",
                "lantern",
                "--app-version",
                "1.0.0",
                "--member",
                "helper:juniper"
            ])
            .status
            .success()
    );
    assert!(
        daemon
            .home
            .run(&["room", "create", "garden", "--title", "Garden"])
            .status
            .success()
    );
    let body = "a".repeat(4096);
    assert!(
        daemon
            .home
            .run(&["post", "garden", "--as", "helper:juniper", "--body", &body])
            .status
            .success()
    );
    let body = "a".repeat(4097);
    let file = daemon.home.0.join("body");
    fs::write(&file, &body).unwrap();
    for output in [
        daemon
            .home
            .run(&["post", "garden", "--as", "helper:juniper", "--body", &body]),
        daemon.home.input(
            &[
                "post",
                "garden",
                "--as",
                "helper:juniper",
                "--body-file",
                "-",
            ],
            body.as_bytes(),
        ),
        daemon.home.run(&[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body-file",
            file.to_str().unwrap(),
        ]),
    ] {
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            output.stdout,
            b"{\"ok\":false,\"error\":\"the message is empty or too long\"}\n"
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn a_body_larger_than_a_request_sends_nothing() {
    let server = StandIn::start(r#"{"ok":true}"#);
    let body = vec![b'a'; radio_client::REQUEST_MAX_BYTES + 1];
    let output = server.home.input(
        &[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body-file",
            "-",
        ],
        &body,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"radio: the message is empty or too long\n");
    server.assert_no_connection();
}

#[test]
fn an_unbounded_body_file_is_rejected_within_five_seconds() {
    let server = StandIn::start(r#"{"ok":true}"#);
    let mut child = server
        .home
        .command(&[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body-file",
            "/dev/zero",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("body-file read did not finish within 5 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"radio: the message is empty or too long\n");
    server.assert_no_connection();
}

#[test]
fn body_file_errors_send_nothing() {
    let server = StandIn::start(r#"{"ok":true}"#);
    let invalid = server.home.0.join("invalid-body");
    fs::write(&invalid, [0xff]).unwrap();
    for (path, stderr) in [
        (
            server.home.0.join("missing-body"),
            "radio: the body file could not be read\n",
        ),
        (invalid, "radio: the body is not UTF-8\n"),
    ] {
        let output = server.home.run(&[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body-file",
            path.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, stderr.as_bytes());
        server.assert_no_connection();
    }
}

#[test]
fn readme_python_example_runs() {
    let binary_folder = Path::new(env!("CARGO_BIN_EXE_radio")).parent().unwrap();
    let python_path = format!("{}:/usr/bin:/bin", binary_folder.display());
    let available = Command::new("python3")
        .arg("--version")
        .env("PATH", &python_path)
        .output();
    if matches!(&available, Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
        assert_ne!(
            std::env::var("RADIO_STRICT").as_deref(),
            Ok("1"),
            "python3 is required in strict mode"
        );
        eprintln!("skipped: no python3");
        return;
    }
    assert!(
        available.unwrap().status.success(),
        "python3 --version failed"
    );
    let daemon = Daemon::start();
    let readme =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let section = readme
        .split_once("### Using Radio from the command line\n")
        .unwrap()
        .1;
    let python = section
        .split_once("```python\n")
        .unwrap()
        .1
        .split_once("```")
        .unwrap()
        .0;
    let script = daemon.home.0.join("example.py");
    fs::write(&script, python).unwrap();
    let output = Command::new("python3")
        .arg(script)
        .env("HOME", &daemon.home.0)
        .env("PATH", &python_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"1 ada The first draft is ready.\n");
    assert!(output.stderr.is_empty());
}
