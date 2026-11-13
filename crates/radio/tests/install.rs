use radio_client::{Request, request, socket_path};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "usage: radio serve [--name <name>]\n       radio install --app <name> --app-version <version> --handler <path> [--rooms <path>]\n       radio uninstall --app <name>\n       radio status\n       radio hello\n       radio register --app <app> --app-version <version> [--member <kind>:<name>]...\n       radio room create <room> --title <title>\n       radio rooms [--after <room>] [--limit <n>]\n       radio post <room> --as <kind>:<name> (--body <text> | --body-file <path|->)\n       radio read <room> [--since <seq>] [--limit <n>]\n       radio line list <room>\n       radio line set <room> <key> <line> --as <kind>:<name> [--expires-at <seconds>]\n       radio line clear <room> <key> --as <kind>:<name>\n";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const LAUNCHCTL: &str = r##"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HOME/launchctl.log"
pidfile="$HOME/radio.pid"
live() {
    [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null
}
stop() {
    if live; then
        kill "$(cat "$pidfile")"
        n=0
        while live && [ "$n" -lt 5 ]; do
            sleep 1
            n=$((n + 1))
        done
    fi
    rm -f "$pidfile"
}
start() {
    [ "${FAKE_BOOTSTRAP_FAILS:-0}" != 1 ] || exit 5
    bin=$(/usr/bin/plutil -extract ProgramArguments.0 raw -o - "$1")
    log=$(/usr/bin/plutil -extract StandardErrorPath raw -o - "$1")
    "$bin" serve --name test-host >> "$log" 2>&1 < /dev/null &
    printf '%s\n' "$!" > "$pidfile"
}
domain() {
    case "$1" in
        gui/*)
            number=${1#gui/}
            case "$number" in ''|*[!0-9]*) return 1 ;; esac
            ;;
        *) return 1 ;;
    esac
}
target() {
    case "$1" in
        gui/*/ai.awna.radio) domain "${1%/ai.awna.radio}" ;;
        *) return 1 ;;
    esac
}
[ "$#" -ge 1 ] || exit 64
case "$1" in
    print)
        [ "$#" -eq 2 ] || exit 64
        if domain "$2"; then exit 0; fi
        target "$2" || exit 64
        live
        ;;
    bootout)
        [ "$#" -eq 2 ] && target "$2" || exit 64
        stop
        ;;
    bootstrap)
        [ "$#" -eq 3 ] && domain "$2" || exit 64
        start "$3"
        ;;
    kickstart)
        [ "$#" -eq 3 ] && [ "$2" = -k ] && target "$3" || exit 64
        stop
        start "$HOME/Library/LaunchAgents/ai.awna.radio.plist"
        ;;
    *) exit 64 ;;
esac
"##;

struct Fixture {
    home: PathBuf,
    old: RefCell<Option<PathBuf>>,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let home = PathBuf::from(format!(
            "/tmp/rd-inst-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&home).unwrap();
        let fixture = Self {
            home,
            old: RefCell::new(None),
        };
        fixture.script("launchctl", LAUNCHCTL);
        fixture.script("handler", "#!/bin/sh\nexit 0\n");
        fixture
    }

    fn script(&self, name: &str, text: &str) {
        let path = self.home.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
        command
            .env("HOME", &self.home)
            .env("RADIO_LAUNCHCTL", self.home.join("launchctl"))
            .env_remove("FAKE_BOOTSTRAP_FAILS");
        command
    }

    fn install_command(&self, app: &str) -> Command {
        let mut command = self.command();
        command
            .args([
                "install",
                "--app",
                app,
                "--app-version",
                "2.1.0",
                "--handler",
            ])
            .arg(self.home.join("handler"));
        command
    }

    fn install(&self, app: &str) -> Output {
        output(self.install_command(app))
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command();
        command.args(args);
        output(command)
    }

    fn state(&self) -> PathBuf {
        self.home.join(".local/state/radio")
    }

    fn agent(&self) -> PathBuf {
        self.home.join("Library/LaunchAgents/ai.awna.radio.plist")
    }

    fn hello(&self) {
        let answer = request(&socket_path(&self.home), &Request::Hello {}).unwrap();
        assert!(answer.ok);
        assert_eq!(answer.radio.as_deref(), Some(VERSION));
    }

    fn pid(&self) -> Option<i32> {
        fs::read_to_string(self.home.join("radio.pid"))
            .ok()?
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|pid| *pid > 1)
    }

    fn old_store(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rd-old-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        *self.old.borrow_mut() = Some(dir.clone());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("rooms.db");
        sql(&path, "CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (2);
            CREATE TABLE rooms (id TEXT, home TEXT, title TEXT, created REAL);
            INSERT INTO rooms VALUES ('porch','node-a','Porch',1);
            CREATE TABLE messages (room TEXT, seq INTEGER, author_host TEXT, author_kind TEXT, author TEXT, body TEXT, sent_at REAL, delivered_at REAL, kind TEXT);
            INSERT INTO messages VALUES ('porch',1,'node-a','human','Wren','hello',2,NULL,'post');
            INSERT INTO messages VALUES ('porch',2,'node-a','human','Wren','welcome',3,NULL,'post');
            CREATE TABLE room_status (room TEXT, key TEXT, line TEXT, set_by_kind TEXT, set_by TEXT, set_at REAL, expires_at REAL);");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            // SAFETY: pid is a positive process id recorded by our stand-in.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let start = Instant::now();
            while live(pid) && start.elapsed() < Duration::from_secs(1) {
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = fs::remove_dir_all(&self.home);
        if let Some(old) = self.old.get_mut().take() {
            let _ = fs::remove_dir_all(old);
        }
    }
}

fn live(pid: i32) -> bool {
    // SAFETY: signal zero only checks the positive process id's existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn output(mut command: Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() >= Duration::from_secs(18) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("radio command exceeded 18 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn sql(path: &Path, text: &str) {
    assert!(
        Command::new("/usr/bin/sqlite3")
            .arg(path)
            .arg(text)
            .status()
            .unwrap()
            .success()
    );
}

fn assert_output(output: &Output, code: i32, stdout: &str, stderr: &str) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, stdout.as_bytes());
    assert_eq!(output.stderr, stderr.as_bytes());
}

#[test]
fn install_status_and_uninstall_by_two_apps() {
    let f = Fixture::new();
    assert_output(
        &f.install("gardenapp"),
        0,
        &format!("radio {VERSION} installed for gardenapp\n"),
        "",
    );
    assert!(
        Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(f.agent())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read(f.home.join(format!(".local/libexec/radio/{VERSION}/radio"))).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_radio")).unwrap()
    );
    let status = f.run(&["status"]);
    assert_eq!(status.status.code(), Some(0));
    assert!(status.stderr.is_empty());
    let status = String::from_utf8(status.stdout).unwrap();
    let lines: Vec<_> = status.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with(&format!("radio {VERSION}, installed by gardenapp at ")));
    assert_eq!(lines[1], "clients: gardenapp 2.1.0");
    assert_eq!(lines[2], format!("socket: answers, radio {VERSION}"));
    f.hello();
    let calls = fs::read_to_string(f.home.join("launchctl.log")).unwrap();
    let restarts = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("bootout ") || line.starts_with("bootstrap "))
            .count()
    };
    assert_output(
        &f.install("kiteapp"),
        0,
        &format!("radio {VERSION} is already installed; kiteapp is now a client\n"),
        "",
    );
    assert_eq!(
        restarts(&calls),
        restarts(&fs::read_to_string(f.home.join("launchctl.log")).unwrap())
    );
    assert_output(
        &f.run(&["uninstall", "--app", "gardenapp"]),
        0,
        "gardenapp removed; radio stays for the other clients\n",
        "",
    );
    f.hello();
    let pid = f.pid().unwrap();
    assert_output(
        &f.run(&["uninstall", "--app", "kiteapp"]),
        0,
        "kiteapp removed; it was the last client, so radio was removed (rooms and other data are kept in ~/.local/state/radio)\n",
        "",
    );
    assert!(!live(pid));
    assert!(request(&socket_path(&f.home), &Request::Hello {}).is_err());
    assert!(!f.agent().exists());
    assert!(!f.home.join(".local/libexec/radio").exists());
    assert!(f.state().join("rooms.db").exists());
    assert_output(
        &f.run(&["status"]),
        1,
        "radio: not installed\nclients: none\nsocket: no answer\n",
        "",
    );
    assert_output(
        &f.run(&["uninstall", "--app", "kiteapp"]),
        0,
        "kiteapp was not a client\n",
        "",
    );
}

#[test]
fn install_imports_an_older_rooms_store() {
    let f = Fixture::new();
    let source = f.old_store();
    let mut command = f.install_command("gardenapp");
    command.arg("--rooms").arg(&source);
    assert_output(
        &output(command),
        0,
        &format!(
            "radio {VERSION} installed for gardenapp\nrooms imported: 1 added, 0 merged, 2 messages, 0 status lines, 0 skipped\n"
        ),
        "",
    );
    let messages = request(
        &socket_path(&f.home),
        &Request::Read {
            room: "porch".into(),
            since: None,
            limit: None,
        },
    )
    .unwrap()
    .messages
    .unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| message.body.as_str())
            .collect::<Vec<_>>(),
        ["hello", "welcome"]
    );
    let mut command = f.install_command("kiteapp");
    command.arg("--rooms").arg(&source);
    assert_output(
        &output(command),
        0,
        &format!(
            "radio {VERSION} is already installed; kiteapp is now a client\nrooms: that store was imported before\n"
        ),
        "",
    );
    let invalid = source.with_file_name("new.db");
    sql(
        &invalid,
        "CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (3);",
    );
    let mut command = f.install_command("kiteapp");
    command.arg("--rooms").arg(invalid);
    assert_output(
        &output(command),
        4,
        &format!("radio {VERSION} is already installed; kiteapp is now a client\n"),
        "radio: installed, but the rooms were not imported: the store to import is not a version this build can import\n",
    );
    f.hello();
}

#[test]
fn install_refuses_an_older_store_under_the_tmp_home() {
    let f = Fixture::new();
    let source = f.old_store();
    let dir = f.home.join("old");
    fs::create_dir(&dir).unwrap();
    let unsafe_source = dir.join("rooms.db");
    fs::copy(source, &unsafe_source).unwrap();
    let mut command = f.install_command("gardenapp");
    command.arg("--rooms").arg(unsafe_source);
    assert_output(
        &output(command),
        4,
        &format!("radio {VERSION} installed for gardenapp\n"),
        "radio: installed, but the rooms were not imported: the store to import can be changed by another user\n",
    );
}

#[test]
fn a_failed_first_start_installs_nothing() {
    let f = Fixture::new();
    let mut command = f.install_command("gardenapp");
    command.env("FAKE_BOOTSTRAP_FAILS", "1");
    assert_output(
        &output(command),
        1,
        "",
        "radio: the Radio did not start, so nothing is installed\n",
    );
    assert!(!f.agent().exists());
    assert!(!f.state().join("install.json").exists());
    assert!(
        !f.home
            .join(format!(".local/libexec/radio/{VERSION}"))
            .exists()
    );
    assert!(!f.state().join("clients/gardenapp.json").exists());
}

#[test]
fn usage_errors_print_only_usage() {
    let f = Fixture::new();
    let valid = vec![
        "install",
        "--app",
        "gardenapp",
        "--app-version",
        "2.1.0",
        "--handler",
        "/unused",
    ];
    let mut cases = vec![
        vec![],
        vec!["serve", "--name"],
        vec!["serve", "--other", "garden"],
        vec!["serve", "--name", "bad/name"],
        vec!["serve", "--name", ""],
        vec!["serve", "--name", "garden", "extra"],
        vec!["install"],
        vec!["install", "--app", "gardenapp", "--app-version", "2.1.0"],
        vec!["install", "--app", "--handler"],
        vec!["uninstall"],
        vec!["uninstall", "--app"],
        vec!["uninstall", "--app", "--other"],
        vec!["uninstall", "--app", "gardenapp", "--app", "kiteapp"],
        vec!["uninstall", "--app", "gardenapp", "extra"],
        vec!["uninstall", "--other", "gardenapp"],
        vec!["status", "extra"],
    ];
    for flag in [
        "--app",
        "--app-version",
        "--handler",
        "--rooms",
        "--unknown",
    ] {
        let mut args = valid.clone();
        if flag == "--rooms" {
            args.extend([flag, "/old.db"]);
        }
        args.extend([flag, "value"]);
        cases.push(args);
        let mut args = valid.clone();
        args.push(flag);
        cases.push(args);
    }
    let mut stray = valid;
    stray.push("extra");
    cases.push(stray);
    for args in cases {
        assert_output(&f.run(&args), 2, "", USAGE);
    }
}

#[test]
fn argument_refusals_exit_2() {
    for (case, phrase) in [
        (0, "the handler is not an absolute path"),
        (1, "the handler can be changed by another user"),
        (2, "not an app name"),
        (3, "not an app name"),
    ] {
        let f = Fixture::new();
        let mut command = f.command();
        let app = match case {
            2 => ".x",
            3 => "Garden",
            _ => "gardenapp",
        };
        let handler = if case == 0 {
            PathBuf::from("handler")
        } else {
            f.home.join("handler")
        };
        if case == 1 {
            fs::set_permissions(&handler, fs::Permissions::from_mode(0o720)).unwrap();
        }
        command
            .args([
                "install",
                "--app",
                app,
                "--app-version",
                "2.1.0",
                "--handler",
            ])
            .arg(handler);
        assert_output(&output(command), 2, "", &format!("radio: {phrase}\n"));
        assert!(!f.home.join("Library").exists());
        assert!(!f.home.join(".local/libexec").exists());
        if f.state().exists() {
            assert!(fs::read_dir(f.state()).unwrap().next().is_none());
        }
    }
}

#[test]
fn a_temp_home_without_a_stand_in_never_reaches_launchctl() {
    let f = Fixture::new();
    let mut command = f.install_command("gardenapp");
    command.env_remove("RADIO_LAUNCHCTL");
    assert_output(
        &output(command),
        1,
        "",
        "radio: HOME is not this account's own, so no real launch agent is loaded from it; set RADIO_LAUNCHCTL to a stand-in\n",
    );
    assert!(!f.state().exists());
    assert!(!f.home.join("launchctl.log").exists());
}

#[test]
fn status_never_resolves_or_runs_launchctl() {
    let f = Fixture::new();
    for fake in [false, true] {
        let mut command = f.command();
        command.arg("status");
        if fake {
            command.env("RADIO_LAUNCHCTL", f.home.join("missing-launchctl"));
        } else {
            command.env_remove("RADIO_LAUNCHCTL");
        }
        assert_output(
            &output(command),
            1,
            "radio: not installed\nclients: none\nsocket: no answer\n",
            "",
        );
    }
    assert!(!f.state().exists());
    assert!(!f.home.join("launchctl.log").exists());
}
