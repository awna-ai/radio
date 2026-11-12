use super::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, PermissionsExt, symlink},
        },
    },
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};

type BootstrapCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    loaded: Option<PathBuf>,
    on_bootstrap: BTreeMap<String, BootstrapCallback>,
    calls: Vec<Vec<String>>,
    fail_bootstrap: BTreeSet<String>,
    never_answers: BTreeSet<String>,
    quiet_until_kickstart: bool,
    stale: bool,
    quiet_calls: usize,
    no_gui: bool,
    delay: bool,
    bootstrap_attempts: usize,
    fail_attempts: usize,
    stuck: bool,
    failed_kickstart: bool,
}

#[derive(Clone, Default)]
struct Fake {
    state: Arc<Mutex<State>>,
    in_flight: Arc<AtomicUsize>,
}

fn binary_version(binary: &Path) -> String {
    binary
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .into()
}

impl Launchd for Fake {
    fn run(&self, args: &[&str]) -> Result<bool, String> {
        if args[0] != "print" || args[1].ends_with(LABEL) {
            assert_eq!(
                self.in_flight.load(Ordering::SeqCst),
                0,
                "a launchd call overlapped bootstrap"
            );
        }
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(match args {
            ["print", domain] if !domain.ends_with(LABEL) => !state.no_gui,
            ["print", _] => state.loaded.is_some(),
            ["bootout", _] => {
                if !state.stuck {
                    state.loaded = None;
                }
                true
            }
            ["bootstrap", _, path] => {
                let text = fs::read_to_string(path).unwrap();
                let escaped = text
                    .split("<array>")
                    .nth(1)
                    .unwrap()
                    .split("<string>")
                    .nth(1)
                    .unwrap()
                    .split("</string>")
                    .next()
                    .unwrap();
                let binary = PathBuf::from(
                    escaped
                        .replace("&apos;", "'")
                        .replace("&quot;", "\"")
                        .replace("&gt;", ">")
                        .replace("&lt;", "<")
                        .replace("&amp;", "&"),
                );
                let log = text
                    .split("<key>StandardOutPath</key>")
                    .nth(1)
                    .unwrap()
                    .split("<string>")
                    .nth(1)
                    .unwrap()
                    .split("</string>")
                    .next()
                    .unwrap()
                    .replace("&apos;", "'")
                    .replace("&quot;", "\"")
                    .replace("&gt;", ">")
                    .replace("&lt;", "<")
                    .replace("&amp;", "&");
                assert_eq!(
                    mode(Path::new(&log)),
                    0o600,
                    "the private log must exist before bootstrap"
                );
                state.bootstrap_attempts += 1;
                if state.fail_bootstrap.contains(&binary_version(&binary))
                    || state.bootstrap_attempts <= state.fail_attempts
                {
                    return Ok(false);
                }
                let callback = state.on_bootstrap.get(&binary_version(&binary)).cloned();
                let delay = state.delay;
                drop(state);
                assert_eq!(self.in_flight.fetch_add(1, Ordering::SeqCst), 0);
                if delay {
                    thread::sleep(Duration::from_millis(50));
                }
                if let Some(callback) = callback {
                    callback();
                }
                self.state.lock().unwrap().loaded = Some(binary);
                assert_eq!(self.in_flight.fetch_sub(1, Ordering::SeqCst), 1);
                true
            }
            ["kickstart", "-k", _] => {
                state.quiet_until_kickstart = false;
                state.loaded.is_some() && !state.failed_kickstart
            }
            _ => panic!("unexpected launchd arguments"),
        })
    }
}

impl Probe for Fake {
    fn hello(&self, _socket: &Path) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        if state.quiet_calls > 0 {
            state.quiet_calls -= 1;
            return None;
        }
        if state.quiet_until_kickstart {
            return None;
        }
        let version = binary_version(state.loaded.as_ref()?);
        if state.stale {
            return Some("1.0.0".into());
        }
        (!state.never_answers.contains(&version)).then_some(version)
    }
}

struct Fixture {
    home: PathBuf,
    fake: Fake,
    hello_wait: Duration,
    hello_poll: Duration,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "radio-install-{}-{}&home",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&home).unwrap();
        let handler = home.join("handler");
        fs::write(&handler, b"handler").unwrap();
        fs::set_permissions(handler, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            home,
            fake: Fake::default(),
            hello_wait: Duration::from_millis(12),
            hello_poll: Duration::from_millis(1),
        }
    }

    fn with_hello_wait(mut self, wait: Duration, poll: Duration) -> Self {
        self.hello_wait = wait;
        self.hello_poll = poll;
        self
    }

    fn paths(&self) -> Paths {
        Paths::new(&self.home).unwrap()
    }

    fn installer(&self, version: &str) -> Installer<'_> {
        let source = self.home.join(format!("source-{version}"));
        fs::write(&source, format!("radio {version}")).unwrap();
        Installer {
            paths: self.paths(),
            launchd: &self.fake,
            probe: &self.fake,
            timing: Timing {
                lock_wait: Duration::from_secs(2),
                lock_poll: Duration::from_millis(1),
                hello_wait: self.hello_wait,
                hello_poll: self.hello_poll,
                bootout_checks: 3,
                bootout_pause: Duration::from_millis(1),
                bootstrap_tries: 3,
                bootstrap_pause: Duration::from_millis(1),
            },
            version: version.into(),
            source,
            uid: uid(),
        }
    }

    fn args(&self, app: &str) -> InstallArgs {
        InstallArgs {
            app: app.into(),
            app_version: "1.0.0".into(),
            handler: self.home.join("handler"),
            rooms: None,
        }
    }

    fn install(&self, version: &str, app: &str) -> InstallOutcome {
        self.installer(version)
            .install(&self.args(app), 1794452827)
            .unwrap()
    }

    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.paths().record).unwrap()).unwrap()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.fake.state.lock().unwrap().calls.clone()
    }

    fn clear_calls(&self) {
        self.fake.state.lock().unwrap().calls.clear();
    }

    fn fail(&self, versions: &[&str]) {
        self.fake.state.lock().unwrap().fail_bootstrap =
            versions.iter().map(|s| s.to_string()).collect();
    }

    fn quiet(&self, versions: &[&str]) {
        self.fake.state.lock().unwrap().never_answers =
            versions.iter().map(|s| s.to_string()).collect();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o777
}

fn error(result: Result<InstallOutcome, Failure>, code: i32, phrase: &str) {
    assert_eq!(
        result.unwrap_err(),
        Failure {
            code,
            message: phrase.into()
        }
    );
}

fn assert_installed(f: &Fixture, version: &str) {
    assert_eq!(f.record()["version"], version);
    assert_eq!(
        f.fake.state.lock().unwrap().loaded,
        Some(f.paths().binary(version))
    );
}

#[test]
fn two_installs_at_once_queue() {
    let f = Fixture::new();
    f.fake.state.lock().unwrap().delay = true;
    let barrier = Barrier::new(2);
    thread::scope(|scope| {
        let a = scope.spawn(|| {
            let installer = f.installer("1.0.0");
            barrier.wait();
            installer.install(&f.args("gardenapp"), 1)
        });
        let b = scope.spawn(|| {
            let installer = f.installer("2.0.0");
            barrier.wait();
            installer.install(&f.args("kiteapp"), 2)
        });
        assert!(a.join().unwrap().is_ok());
        assert!(b.join().unwrap().is_ok());
    });
    assert_installed(&f, "2.0.0");
    let previous = f.record()["previous"]["version"]
        .as_str()
        .map(str::to_owned);
    let versions: BTreeSet<_> = fs::read_dir(f.paths().binaries)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let expected: BTreeSet<_> = previous.into_iter().chain(["2.0.0".into()]).collect();
    assert_eq!(versions, expected);
    assert!(f.paths().clients.join("gardenapp.json").is_file());
    assert!(f.paths().clients.join("kiteapp.json").is_file());
}

#[test]
fn a_held_lock_times_out_after_only_the_gui_check() {
    let f = Fixture::new();
    radio_rooms::private_dir(&f.paths().state, true).unwrap();
    let lock = fs::File::create(f.paths().lock).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let mut i = f.installer("1.0.0");
    i.timing.lock_wait = Duration::from_millis(10);
    let start = Instant::now();
    error(
        i.install(&f.args("gardenapp"), 1),
        1,
        "another install did not finish in time",
    );
    assert!(start.elapsed() >= i.timing.lock_wait);
    assert_eq!(
        f.calls(),
        vec![vec!["print".to_owned(), format!("gui/{}", uid())]]
    );
}

#[test]
fn an_older_build_never_replaces_a_newer_one() {
    let f = Fixture::new();
    f.install("2.0.0", "kiteapp");
    let record = fs::read(f.paths().record).unwrap();
    let agent = fs::read(f.paths().agent).unwrap();
    f.clear_calls();
    assert_eq!(
        f.install("1.0.0", "gardenapp").installed,
        Installed::Kept {
            installed: "2.0.0".into()
        }
    );
    assert_eq!(fs::read(f.paths().record).unwrap(), record);
    assert_eq!(fs::read(f.paths().agent).unwrap(), agent);
    assert!(!f.paths().binary("1.0.0").exists());
    assert!(f.calls().iter().all(|c| c[0] == "print"));
    assert!(f.paths().clients.join("gardenapp.json").is_file());
}

fn rollback_case(bootstrap: bool, quiet: bool, rollback_fails: bool) {
    let f = Fixture::new();
    f.install("1.0.0", "gardenapp");
    let record = fs::read(f.paths().record).unwrap();
    if bootstrap {
        f.fail(if rollback_fails {
            &["1.0.0", "2.0.0"]
        } else {
            &["2.0.0"]
        });
    }
    if quiet {
        f.quiet(&["2.0.0"]);
    }
    error(
        f.installer("2.0.0").install(&f.args("kiteapp"), 2),
        1,
        if rollback_fails {
            "the upgrade failed, and the version before it did not start again"
        } else {
            "the upgrade failed; the version before it is running again"
        },
    );
    assert_eq!(fs::read(f.paths().record).unwrap(), record);
    assert!(!f.paths().binary("2.0.0").parent().unwrap().exists());
    assert_eq!(
        fs::read_to_string(f.paths().agent).unwrap(),
        plist(&f.paths().binary("1.0.0"), &f.paths().log)
    );
    if !rollback_fails {
        assert_installed(&f, "1.0.0");
    }
    assert!(!f.paths().clients.join("kiteapp.json").exists());
}

#[test]
fn a_failed_bootstrap_rolls_back() {
    rollback_case(true, false, false);
}

#[test]
fn an_upgrade_that_never_answers_rolls_back() {
    rollback_case(false, true, false);
}

#[test]
fn a_rollback_that_fails_says_so() {
    rollback_case(true, false, true);
}

#[test]
fn a_fresh_install_places_writes_and_starts() {
    let f = Fixture::new();
    let out = f.install("1.0.0", "kiteapp");
    assert_eq!(
        out.installed,
        Installed::Fresh {
            version: "1.0.0".into()
        }
    );
    assert!(!out.record_unreadable && !out.binary_changed);
    assert!(out.rooms.is_none());
    let p = f.paths();
    assert_eq!(fs::read(p.binary("1.0.0")).unwrap(), b"radio 1.0.0");
    for path in [
        &p.state,
        &p.clients,
        &p.binaries,
        p.binary("1.0.0").parent().unwrap(),
        &p.binary("1.0.0"),
    ] {
        assert_eq!(mode(path), 0o700);
    }
    for path in [&p.log, &p.lock, &p.record, &p.clients.join("kiteapp.json")] {
        assert_eq!(mode(path), 0o600);
    }
    assert_eq!(mode(&p.agent), 0o644);
    assert_eq!(mode(p.binaries.parent().unwrap()), 0o755);
    let r = f.record();
    assert_eq!(r["installed_by"], "kiteapp");
    assert_eq!(r["at"], 1794452827u64);
    assert!(r["previous"].is_null());
    assert_eq!(r["path"], p.binary("1.0.0").to_str().unwrap());
    assert_eq!(
        r["sha256"],
        "68b85139446bbc77068b32b17fa23c52e1f8bdf45dde33cc6a1aa6564e07e0fa"
    );
    assert_eq!(
        fs::read_to_string(&p.agent).unwrap(),
        plist(&p.binary("1.0.0"), &p.log)
    );
    let client: Value =
        serde_json::from_slice(&fs::read(p.clients.join("kiteapp.json")).unwrap()).unwrap();
    assert_eq!(
        client,
        json!({"app":"kiteapp","app_version":"1.0.0","handler":fs::canonicalize(f.home.join("handler")).unwrap(),"majors":[1],"at":1794452827u64})
    );
    assert_installed(&f, "1.0.0");
}

#[test]
fn a_fresh_install_that_does_not_start_leaves_nothing() {
    let f = Fixture::new();
    f.fail(&["1.0.0"]);
    error(
        f.installer("1.0.0").install(&f.args("kiteapp"), 1),
        1,
        "the Radio did not start, so nothing is installed",
    );
    let p = f.paths();
    assert!(!p.record.exists());
    assert!(!p.agent.exists());
    assert!(!p.binary("1.0.0").parent().unwrap().exists());
    assert!(!p.clients.join("kiteapp.json").exists());
    assert!(f.fake.state.lock().unwrap().loaded.is_none());
}

#[test]
fn an_upgrade_keeps_one_previous_version() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let p = f.paths();
    fs::create_dir(p.binaries.join("notes")).unwrap();
    symlink(f.home.join("handler"), p.binaries.join("9.0.0")).unwrap();
    f.install("2.0.0", "kiteapp");
    assert_eq!(
        f.install("3.0.0", "kiteapp").installed,
        Installed::Upgraded {
            from: "2.0.0".into(),
            to: "3.0.0".into()
        }
    );
    assert_eq!(f.record()["previous"]["version"], "2.0.0");
    assert!(!p.binary("1.0.0").parent().unwrap().exists());
    assert!(p.binary("2.0.0").exists());
    assert!(p.binary("3.0.0").exists());
    assert!(p.binaries.join("notes").is_dir());
    assert!(
        fs::symlink_metadata(p.binaries.join("9.0.0"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn a_quiet_daemon_is_kickstarted_once() {
    let f = Fixture::new();
    f.install("2.0.0", "kiteapp");
    f.clear_calls();
    f.fake.state.lock().unwrap().quiet_until_kickstart = true;
    f.install("1.0.0", "gardenapp");
    assert_eq!(f.calls().iter().filter(|c| c[0] == "kickstart").count(), 1);
    assert!(
        f.calls()
            .iter()
            .all(|c| !matches!(c[0].as_str(), "bootout" | "bootstrap"))
    );
}

#[test]
fn an_unloaded_daemon_is_bootstrapped_again() {
    let f = Fixture::new();
    f.install("2.0.0", "kiteapp");
    f.fake.state.lock().unwrap().loaded = None;
    f.clear_calls();
    fs::remove_file(f.paths().agent).unwrap();
    f.install("1.0.0", "gardenapp");
    assert_installed(&f, "2.0.0");
    assert_eq!(f.calls().iter().filter(|c| c[0] == "bootstrap").count(), 1);
}

#[test]
fn a_daemon_that_stays_quiet_fails_without_a_client_record() {
    let f = Fixture::new();
    f.install("2.0.0", "kiteapp");
    f.quiet(&["2.0.0"]);
    error(
        f.installer("1.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the installed Radio does not answer",
    );
    assert!(!f.paths().clients.join("gardenapp.json").exists());
}

#[test]
fn a_changed_or_missing_binary_or_bad_record_reinstalls() {
    for case in 0..8 {
        let f = Fixture::new();
        f.install("1.0.0", "kiteapp");
        let p = f.paths();
        match case {
            0 => fs::write(p.binary("1.0.0"), b"changed").unwrap(),
            1 => fs::remove_file(p.binary("1.0.0")).unwrap(),
            2 => fs::write(&p.record, b"not json").unwrap(),
            3..=5 => {
                let mut r = f.record();
                if case == 3 {
                    r["unknown"] = json!(true);
                } else if case == 4 {
                    r["path"] = json!("/elsewhere/radio");
                } else {
                    r["sha256"] = json!("F".repeat(64));
                }
                fs::write(&p.record, serde_json::to_vec(&r).unwrap()).unwrap();
            }
            6 => {
                fs::remove_file(&p.record).unwrap();
                symlink(f.home.join("handler"), &p.record).unwrap();
            }
            _ => {
                fs::remove_file(p.binary("1.0.0")).unwrap();
                symlink(f.home.join("handler"), p.binary("1.0.0")).unwrap();
            }
        }
        let out = f.install("2.0.0", "gardenapp");
        assert_eq!(
            out.installed,
            Installed::Fresh {
                version: "2.0.0".into()
            }
        );
        assert_eq!(out.binary_changed, matches!(case, 0 | 1 | 7));
        assert_eq!(out.record_unreadable, matches!(case, 2..=6));
        assert_installed(&f, "2.0.0");
        let versions: Vec<_> = fs::read_dir(&p.binaries)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(versions, [OsString::from("2.0.0")]);
    }
}

#[test]
fn argument_refusals_write_nothing() {
    for name in ["", ".hidden", "a/b", "Garden", &"a".repeat(65)] {
        let f = Fixture::new();
        error(
            f.installer("1.0.0").install(&f.args(name), 1),
            2,
            "not an app name",
        );
        assert!(!f.paths().state.exists());
        assert!(f.calls().is_empty());
    }
    for version in ["", "a/b", "é", &"a".repeat(65)] {
        let f = Fixture::new();
        let mut args = f.args("kiteapp");
        args.app_version = version.into();
        error(f.installer("1.0.0").install(&args, 1), 2, "not a version");
        assert!(!f.paths().state.exists());
    }
    for (case, phrase) in [
        (0, "the handler is not an absolute path"),
        (1, "the handler is not an executable file"),
        (2, "the handler is not an executable file"),
        (3, "the handler can be changed by another user"),
        (4, "the store to import is not an absolute path"),
        (5, "the store to import is the Radio's own"),
    ] {
        let f = Fixture::new();
        let mut args = f.args("kiteapp");
        match case {
            0 => args.handler = "handler".into(),
            1 => args.handler = f.home.join("missing"),
            2 => fs::set_permissions(&args.handler, fs::Permissions::from_mode(0o600)).unwrap(),
            3 => fs::set_permissions(&args.handler, fs::Permissions::from_mode(0o720)).unwrap(),
            4 => args.rooms = Some("rooms.db".into()),
            _ => args.rooms = Some(f.paths().rooms),
        }
        error(f.installer("1.0.0").install(&args, 1), 2, phrase);
        assert!(!f.paths().state.exists());
        assert!(f.calls().is_empty());
    }
}

#[test]
fn no_gui_session_writes_nothing() {
    let f = Fixture::new();
    f.fake.state.lock().unwrap().no_gui = true;
    error(
        f.installer("1.0.0").install(&f.args("kiteapp"), 1),
        3,
        "no GUI session for this user",
    );
    assert!(!f.paths().state.exists());
}

#[test]
fn an_invalid_build_version_writes_nothing() {
    let f = Fixture::new();
    error(
        f.installer("1.0.0-preview").install(&f.args("kiteapp"), 1),
        1,
        "this build has no valid version",
    );
    assert!(!f.paths().state.exists());
}

#[test]
fn rooms_import_once_and_failure_keeps_the_install() {
    let f = Fixture::new();
    let source = f.home.join("old.db");
    let sql = "CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (2);
        CREATE TABLE rooms (id TEXT, home TEXT, title TEXT, created REAL); INSERT INTO rooms VALUES ('porch','node-a','Porch',1);
        CREATE TABLE messages (room TEXT, seq INTEGER, author_host TEXT, author_kind TEXT, author TEXT, body TEXT, sent_at REAL, delivered_at REAL, kind TEXT);
        INSERT INTO messages VALUES ('porch',1,'node-a','human','Wren','hello',2,NULL,'post');
        CREATE TABLE room_status (room TEXT, key TEXT, line TEXT, set_by_kind TEXT, set_by TEXT, set_at REAL, expires_at REAL);";
    assert!(
        std::process::Command::new("/usr/bin/sqlite3")
            .arg(&source)
            .arg(sql)
            .status()
            .unwrap()
            .success()
    );
    let mut args = f.args("kiteapp");
    args.rooms = Some(source.clone());
    let i = f.installer("1.0.0");
    assert_eq!(
        i.install(&args, 3).unwrap().rooms,
        Some(RoomsOutcome::Imported(radio_rooms::ImportReport {
            rooms_added: 1,
            rooms_merged: 0,
            messages: 1,
            statuses: 0,
            skipped: 0
        }))
    );
    assert_eq!(
        i.install(&args, 4).unwrap().rooms,
        Some(RoomsOutcome::AlreadyImported)
    );
    let invalid = f.home.join("new.db");
    assert!(std::process::Command::new("/usr/bin/sqlite3").arg(&invalid).arg("CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (3);").status().unwrap().success());
    args.rooms = Some(invalid);
    assert_eq!(
        i.install(&args, 5).unwrap().rooms,
        Some(RoomsOutcome::Failed(
            "the store to import is not a version this build can import".into()
        ))
    );
    args.rooms = Some(f.home.join("missing.db"));
    assert!(matches!(
        i.install(&args, 6).unwrap().rooms,
        Some(RoomsOutcome::Failed(_))
    ));
    assert_installed(&f, "1.0.0");
}

#[test]
fn uninstall_keeps_data_and_waits_for_the_last_client() {
    let f = Fixture::new();
    f.install("1.0.0", "gardenapp");
    f.install("2.0.0", "kiteapp");
    let p = f.paths();
    fs::write(&p.rooms, b"rooms kept").unwrap();
    fs::write(p.state.join("rooms.db-wal"), b"wal kept").unwrap();
    fs::write(p.state.join("rooms.db-shm"), b"shm kept").unwrap();
    fs::create_dir(p.binaries.join("notes")).unwrap();
    symlink(f.home.join("handler"), p.binaries.join("4.0.0")).unwrap();
    f.clear_calls();
    let i = f.installer("2.0.0");
    assert_eq!(
        i.uninstall("gardenapp").unwrap(),
        Uninstalled::Removed { last: false }
    );
    assert_installed(&f, "2.0.0");
    assert!(p.binary("1.0.0").exists());
    assert_eq!(
        i.uninstall("kiteapp").unwrap(),
        Uninstalled::Removed { last: true }
    );
    assert!(f.fake.state.lock().unwrap().loaded.is_none());
    assert!(!p.agent.exists() && !p.record.exists());
    assert!(
        !p.binary("1.0.0").parent().unwrap().exists()
            && !p.binary("2.0.0").parent().unwrap().exists()
    );
    assert!(p.clients.is_dir() && p.log.is_file() && p.lock.is_file());
    assert_eq!(fs::read(p.rooms).unwrap(), b"rooms kept");
    assert_eq!(fs::read(p.state.join("rooms.db-wal")).unwrap(), b"wal kept");
    assert_eq!(fs::read(p.state.join("rooms.db-shm")).unwrap(), b"shm kept");
    assert!(p.binaries.join("notes").exists());
    assert!(
        fs::symlink_metadata(p.binaries.join("4.0.0"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn uninstall_unknown_and_unreadable_clients_are_conservative() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let i = f.installer("1.0.0");
    f.clear_calls();
    assert_eq!(i.uninstall("gardenapp").unwrap(), Uninstalled::NotAClient);
    assert!(f.calls().is_empty());
    fs::write(f.paths().clients.join("x.json"), b"not json").unwrap();
    assert_eq!(
        i.uninstall("kiteapp").unwrap(),
        Uninstalled::Removed { last: false }
    );
    assert_installed(&f, "1.0.0");
    assert_eq!(
        i.uninstall("../x").unwrap_err(),
        Failure {
            code: 2,
            message: "not an app name".into()
        }
    );
}

#[test]
fn uninstall_removes_an_empty_binaries_directory() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    assert_eq!(
        f.installer("1.0.0").uninstall("kiteapp").unwrap(),
        Uninstalled::Removed { last: true }
    );
    assert!(!f.paths().binaries.exists());
}

#[test]
fn status_renders_only_validated_records() {
    let f = Fixture::new();
    let i = f.installer("1.0.0");
    assert_eq!(
        render(&i.status()),
        "radio: not installed\nclients: none\nsocket: no answer\n"
    );
    assert!(!i.status().ok);
    assert!(!f.paths().state.exists());
    f.install("1.0.0", "kiteapp");
    let mut args = f.args("gardenapp");
    args.app_version = "2.1.0".into();
    i.install(&args, 1794452827).unwrap();
    assert_eq!(
        render(&i.status()),
        "radio 1.0.0, installed by kiteapp at 2026-11-12T03:07:07Z\nclients: gardenapp 2.1.0, kiteapp 1.0.0\nsocket: answers, radio 1.0.0\n"
    );
    assert!(i.status().ok);
    fs::write(f.paths().clients.join("hello.json"), b"not json").unwrap();
    f.quiet(&["1.0.0"]);
    assert_eq!(
        render(&i.status()),
        "radio 1.0.0, installed by kiteapp at 2026-11-12T03:07:07Z\nclients: gardenapp 2.1.0, (an unreadable record), kiteapp 1.0.0\nsocket: no answer\n"
    );
    assert!(!i.status().ok);
    let mut r = f.record();
    r["installed_by"] = json!("bad\ntext");
    fs::write(f.paths().record, serde_json::to_vec(&r).unwrap()).unwrap();
    assert!(render(&i.status()).starts_with("radio: not installed\n"));
}

#[test]
fn utc_dates_include_the_epoch_and_a_leap_day() {
    assert_eq!(utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc(951782400), "2000-02-29T00:00:00Z");
    assert_eq!(utc(1794452827), "2026-11-12T03:07:07Z");
}

#[test]
fn versions_are_exact_ascii_triples() {
    assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    assert_eq!(parse_version("0.0.0"), Some((0, 0, 0)));
    assert_eq!(
        parse_version("18446744073709551615.0.0"),
        Some((u64::MAX, 0, 0))
    );
    for v in [
        "",
        "0.0.01",
        "1.02.3",
        "0001.02.0",
        "1.2",
        "1.2.3.4",
        "+1.2.3",
        "1.2.3-a",
        "1.2.3+b",
        "١.2.3",
        "1.2.3\n",
        "18446744073709551616.0.0",
        "000000000000000000000.0.0",
    ] {
        assert_eq!(parse_version(v), None);
    }
}

#[test]
fn launchctl_resolve_refuses_another_home_and_takes_a_fake() {
    let f = Fixture::new();
    assert_eq!(Launchctl::resolve(&f.home, None).err(), Some("HOME is not this account's own, so no real launch agent is loaded from it; set RADIO_LAUNCHCTL to a stand-in".into()));
    assert_eq!(
        Launchctl::resolve(&f.home, Some(OsString::new())).err(),
        Launchctl::resolve(&f.home, None).err()
    );
    let program = f.home.join("fake-launchctl");
    fs::write(
        &program,
        b"#!/bin/sh\n[ \"$1\" = print ] && [ \"$2\" = garden ]\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let fake = Launchctl::resolve(&f.home, Some(program.into_os_string())).unwrap();
    assert!(fake.run(&["print", "garden"]).unwrap());
    assert!(!fake.run(&["other"]).unwrap());
    let missing =
        Launchctl::resolve(&f.home, Some(f.home.join("missing").into_os_string())).unwrap();
    assert_eq!(missing.run(&[]), Err("launchctl could not be run".into()));
}

#[test]
fn paths_require_an_absolute_plain_home() {
    assert_eq!(
        Paths::new(Path::new("relative")).unwrap_err(),
        "HOME is not a plain path"
    );
    assert_eq!(
        Paths::new(&PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()))).unwrap_err(),
        "HOME is not a plain path"
    );
    assert_eq!(
        Paths::new(Path::new("/tmp/garden")).unwrap().rooms,
        Path::new("/tmp/garden/.local/state/radio/rooms.db")
    );
}

#[test]
fn binary_replacement_changes_the_inode_and_preserves_open_bytes() {
    let f = Fixture::new();
    let i = f.installer("1.0.0");
    i.place_binary(&i.source, "1.0.0").unwrap();
    let binary = f.paths().binary("1.0.0");
    let mut old = fs::File::open(&binary).unwrap();
    let inode = old.metadata().unwrap().ino();
    fs::write(&i.source, b"new bytes").unwrap();
    i.place_binary(&i.source, "1.0.0").unwrap();
    assert_ne!(inode, fs::metadata(&binary).unwrap().ino());
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"radio 1.0.0");
    assert_eq!(fs::read(binary).unwrap(), b"new bytes");
}

#[test]
fn plist_escapes_xml_and_lints() {
    let f = Fixture::new();
    let binary = f.home.join("a&<>'\"/radio");
    let log = f.home.join("a&log");
    let text = plist(&binary, &log);
    assert!(text.contains("a&amp;&lt;&gt;&apos;&quot;/radio"));
    assert!(text.contains("a&amp;log"));
    assert!(text.contains("\t<integer>63</integer>"));
    let file = f.home.join("agent.plist");
    fs::write(&file, text).unwrap();
    if Path::new("/usr/bin/plutil").exists() {
        assert!(
            std::process::Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(file)
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn a_kickstart_exit_does_not_override_an_answering_daemon() {
    let f = Fixture::new();
    f.install("2.0.0", "kiteapp");
    {
        let mut state = f.fake.state.lock().unwrap();
        state.quiet_until_kickstart = true;
        state.failed_kickstart = true;
    }
    assert!(
        f.installer("1.0.0")
            .install(&f.args("gardenapp"), 2)
            .is_ok()
    );
    assert_installed(&f, "2.0.0");
}

#[test]
fn handler_links_and_aliased_store_parents_follow_the_argument_rules() {
    let f = Fixture::new();
    let alias = f.home.join("handler-alias");
    symlink(f.home.join("handler"), &alias).unwrap();
    let mut args = f.args("kiteapp");
    args.handler = alias;
    f.installer("1.0.0").install(&args, 1).unwrap();
    let client: Value =
        serde_json::from_slice(&fs::read(f.paths().clients.join("kiteapp.json")).unwrap()).unwrap();
    assert_eq!(
        client["handler"],
        fs::canonicalize(f.home.join("handler"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    let alias = f.home.join("state-alias");
    symlink(f.paths().state, &alias).unwrap();
    args.handler = f.home.join("handler");
    args.rooms = Some(alias.join("rooms.db"));
    error(
        f.installer("1.0.0").install(&args, 3),
        2,
        "the store to import is the Radio's own",
    );
    if uid() != 0 {
        let mut installer = f.installer("1.0.0");
        installer.uid = uid() + 1;
        args.rooms = None;
        error(
            installer.install(&args, 4),
            2,
            "the handler can be changed by another user",
        );
    }
}

#[test]
fn lock_and_log_links_are_refused_without_changing_their_targets() {
    for name in ["install.lock", "radio.log"] {
        let f = Fixture::new();
        radio_rooms::private_dir(&f.paths().state, true).unwrap();
        symlink(f.home.join("handler"), f.paths().state.join(name)).unwrap();
        assert!(f.installer("1.0.0").install(&f.args("kiteapp"), 1).is_err());
        assert_eq!(fs::read(f.home.join("handler")).unwrap(), b"handler");
        assert_eq!(mode(&f.home.join("handler")), 0o700);
        assert!(!f.paths().record.exists());
        assert!(f.calls().iter().all(|call| call[0] != "bootstrap"));
    }
}

#[test]
fn stale_temp_files_are_never_overwritten_or_unlinked() {
    let f = Fixture::new();
    let i = f.installer("1.0.0");
    let dir = f.paths().binary("1.0.0").parent().unwrap().to_owned();
    fs::create_dir_all(&dir).unwrap();
    let temp = dir.join(format!("radio.{}.tmp", std::process::id()));
    fs::write(&temp, b"keep").unwrap();
    assert!(i.place_binary(&i.source, "1.0.0").is_err());
    assert_eq!(fs::read(&temp).unwrap(), b"keep");
    assert!(!f.paths().binary("1.0.0").exists());
}

#[test]
fn an_existing_libexec_directory_keeps_its_mode() {
    let f = Fixture::new();
    let dir = f.paths().binaries.parent().unwrap().to_owned();
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();
    f.install("1.0.0", "kiteapp");
    assert_eq!(mode(&dir), 0o750);
}

#[test]
fn bootstrap_retries_and_bootout_waits_are_bounded() {
    let f = Fixture::new();
    f.fake.state.lock().unwrap().fail_attempts = 2;
    f.install("1.0.0", "kiteapp");
    assert_eq!(f.fake.state.lock().unwrap().bootstrap_attempts, 3);
    f.clear_calls();
    f.fake.state.lock().unwrap().stuck = true;
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed, and the version before it did not start again",
    );
    assert!(f.calls().iter().all(|call| call[0] != "bootstrap"));
    assert_eq!(
        f.calls()
            .iter()
            .filter(|call| call[0] == "print" && call[1].ends_with(LABEL))
            .count(),
        6
    );
    assert!(!f.paths().clients.join("gardenapp.json").exists());
}

#[test]
fn status_refuses_unknown_fields_and_invalid_client_values() {
    for (field, value) in [
        ("unknown", json!(true)),
        ("app", json!("kiteapp\nforged")),
        ("app_version", json!("1\nforged")),
        ("majors", json!([2])),
        ("handler", json!("relative")),
    ] {
        let f = Fixture::new();
        f.install("1.0.0", "kiteapp");
        let path = f.paths().clients.join("kiteapp.json");
        let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record[field] = value;
        fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(
            f.installer("1.0.0").status().clients,
            ["(an unreadable record)"]
        );
    }
}

#[test]
fn a_failed_stop_keeps_the_last_client_for_a_retry() {
    let f = Fixture::new();
    f.install("1.0.0", "gardenapp");
    let p = f.paths();
    let client = p.clients.join("gardenapp.json");
    let before = fs::read(&client).unwrap();
    f.fake.state.lock().unwrap().stuck = true;
    assert_eq!(
        f.installer("1.0.0").uninstall("gardenapp").unwrap_err(),
        Failure {
            code: 1,
            message: "the Radio could not be stopped".into()
        }
    );
    assert!(
        client.is_file(),
        "a failed stop must preserve the last client"
    );
    assert_eq!(fs::read(&client).unwrap(), before);
    assert_installed(&f, "1.0.0");
    f.fake.state.lock().unwrap().stuck = false;
    assert_eq!(
        f.installer("1.0.0").uninstall("gardenapp").unwrap(),
        Uninstalled::Removed { last: true }
    );
    assert!(!client.exists());
    assert!(!p.record.exists());
    assert!(!p.agent.exists());
    assert!(!p.binaries.exists());
    assert!(f.fake.state.lock().unwrap().loaded.is_none());
}

#[test]
fn uppercase_uninstall_is_refused() {
    let f = Fixture::new();
    assert_eq!(
        f.installer("1.0.0").uninstall("Garden").unwrap_err(),
        Failure::new(2, "not an app name")
    );
    assert!(!f.paths().state.exists());
}

fn write_upgrade_message(path: &Path) {
    let mut store = radio_rooms::RoomStore::open(path).unwrap();
    store.ensure_room("porch", "node-a", "Porch", 1.0).unwrap();
    store
        .post(
            "porch",
            "node-a",
            &radio_rooms::Author::human("Wren"),
            "newer daemon's write",
            2.0,
        )
        .unwrap();
}

#[test]
fn a_failed_upgrade_restores_the_rooms_store() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let path = f.paths().rooms;
    {
        let mut store = radio_rooms::RoomStore::open(&path).unwrap();
        store.create_room("porch", "node-a", "Porch", 1.0).unwrap();
        store
            .post(
                "porch",
                "node-a",
                &radio_rooms::Author::human("Wren"),
                "before",
                1.0,
            )
            .unwrap();
    }
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(move || write_upgrade_message(&path)),
    );
    f.quiet(&["2.0.0"]);
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed; the version before it is running again",
    );
    let store = radio_rooms::RoomStore::open(&f.paths().rooms).unwrap();
    let messages = store.tail("porch", 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "before");
    assert!(!f.paths().state.join("rooms.db.before-upgrade").exists());
    assert_installed(&f, "1.0.0");
}

#[test]
fn a_failed_upgrade_removes_a_store_that_did_not_exist_before() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let path = f.paths().rooms;
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(move || write_upgrade_message(&path)),
    );
    f.quiet(&["2.0.0"]);
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed; the version before it is running again",
    );
    for name in [
        "rooms.db",
        "rooms.db-wal",
        "rooms.db-shm",
        "rooms.db.before-upgrade",
    ] {
        assert!(
            !f.paths().state.join(name).exists(),
            "{name} was left behind"
        );
    }
    assert_installed(&f, "1.0.0");
}

#[test]
fn an_upgrade_answered_only_by_the_old_version_rolls_back() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    f.fake.state.lock().unwrap().stale = true;
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed; the version before it is running again",
    );
    assert_installed(&f, "1.0.0");
    assert!(!f.paths().clients.join("gardenapp.json").exists());
}

#[test]
fn handlers_in_folders_other_users_can_write_are_refused() {
    for mode in [0o777, 0o775] {
        let f = Fixture::new();
        let dir = f.home.join("writable");
        fs::create_dir(&dir).unwrap();
        let handler = dir.join("handler");
        fs::copy(f.home.join("handler"), &handler).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
        let mut args = f.args("kiteapp");
        args.handler = handler;
        error(
            f.installer("1.0.0").install(&args, 1),
            2,
            "the handler can be changed by another user",
        );
        assert!(!f.paths().state.exists());
    }
}

#[test]
fn handler_folder_permissions() {
    for (owner, group, mode, allowed) in [
        (501, 20, 0o755, true),
        (0, 80, 0o775, true),
        (0, 20, 0o775, false),
        (502, 20, 0o755, false),
        (0, 0, 0o1777, true),
        (0, 0, 0o777, false),
    ] {
        assert_eq!(handler_folder_safe(owner, group, mode, 501), allowed);
    }
}

#[test]
fn a_newer_record_with_a_missing_or_changed_binary_refuses_older_installs() {
    for missing in [true, false] {
        let f = Fixture::new();
        f.install("2.0.0", "kiteapp");
        let p = f.paths();
        if missing {
            fs::remove_file(p.binary("2.0.0")).unwrap();
        } else {
            fs::write(p.binary("2.0.0"), b"changed").unwrap();
        }
        let record = fs::read(&p.record).unwrap();
        let agent = fs::read(&p.agent).unwrap();
        f.clear_calls();
        error(
            f.installer("1.0.0").install(&f.args("gardenapp"), 2),
            1,
            "the installed Radio is newer than this copy and its binary is missing or changed",
        );
        assert_eq!(fs::read(&p.record).unwrap(), record);
        assert_eq!(fs::read(&p.agent).unwrap(), agent);
        assert!(!p.binary("1.0.0").exists());
        assert!(!p.clients.join("gardenapp.json").exists());
        assert!(f.calls().iter().all(|c| c[0] == "print"));
    }
}

#[test]
fn a_missing_or_damaged_record_with_a_newer_version_refuses_older_installs() {
    for damage in 0..3 {
        let f = Fixture::new();
        f.install("2.0.0", "kiteapp");
        let p = f.paths();
        match damage {
            0 => fs::remove_file(&p.record).unwrap(),
            1 => fs::write(&p.record, b"unreadable").unwrap(),
            _ => {
                let mut record = f.record();
                record["future"] = json!(true);
                fs::write(&p.record, serde_json::to_vec(&record).unwrap()).unwrap();
            }
        }
        let agent = fs::read(&p.agent).unwrap();
        error(
            f.installer("1.0.0").install(&f.args("gardenapp"), 2),
            1,
            "a newer Radio is installed here and its record is missing or damaged",
        );
        assert!(p.binary("2.0.0").exists());
        assert_eq!(fs::read(&p.agent).unwrap(), agent);
        assert!(!p.binary("1.0.0").parent().unwrap().exists());
    }
}

#[test]
fn a_fresh_repair_removes_older_leftover_directories() {
    let f = Fixture::new();
    let p = f.paths();
    fs::create_dir_all(p.binary("0.1.0").parent().unwrap()).unwrap();
    f.install("1.0.0", "gardenapp");
    assert_installed(&f, "1.0.0");
    assert!(!p.binary("0.1.0").parent().unwrap().exists());
}

#[test]
fn a_store_newer_than_this_build_refuses_a_fresh_install() {
    let f = Fixture::new();
    let p = f.paths();
    drop(radio_rooms::RoomStore::open(&p.rooms).unwrap());
    let conn = rusqlite::Connection::open(&p.rooms).unwrap();
    conn.execute("UPDATE schema_version SET version = version + 1", [])
        .unwrap();
    drop(conn);
    let before = fs::read(&p.rooms).unwrap();
    error(
        f.installer("1.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the room store is newer than this build",
    );
    assert_eq!(fs::read(&p.rooms).unwrap(), before);
    assert!(!p.binary("1.0.0").parent().unwrap().exists());
    assert!(!p.agent.exists());
    assert!(!p.record.exists());
}

#[test]
fn a_failed_fresh_start_that_cannot_stop_removes_nothing() {
    let f = Fixture::new();
    f.quiet(&["1.0.0"]);
    let path = f.paths().binary("1.0.0");
    let state = Arc::downgrade(&f.fake.state);
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "1.0.0".into(),
        Arc::new(move || {
            state.upgrade().unwrap().lock().unwrap().stuck = true;
        }),
    );
    error(
        f.installer("1.0.0").install(&f.args("kiteapp"), 1),
        1,
        "the Radio did not start and could not be stopped; nothing was removed",
    );
    assert!(path.exists());
    assert!(f.paths().agent.exists());
}

#[test]
fn a_failed_fresh_start_keeps_a_preexisting_version_directory() {
    let f = Fixture::new();
    let dir = f.paths().binary("1.0.0").parent().unwrap().to_owned();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("keep"), b"keep").unwrap();
    f.quiet(&["1.0.0"]);
    error(
        f.installer("1.0.0").install(&f.args("kiteapp"), 1),
        1,
        FRESH_FAILED,
    );
    assert_eq!(fs::read(dir.join("keep")).unwrap(), b"keep");
}

#[test]
fn a_busy_daemon_is_given_the_whole_wait_before_kickstart() {
    let f = Fixture::new().with_hello_wait(Duration::from_secs(2), Duration::from_millis(10));
    f.install("2.0.0", "kiteapp");
    f.clear_calls();
    f.fake.state.lock().unwrap().quiet_calls = 2;
    f.install("1.0.0", "gardenapp");
    assert!(
        f.calls().iter().all(|c| c[0] == "print"),
        "a busy daemon was restarted"
    );
    assert_installed(&f, "2.0.0");
}

#[test]
fn linked_binary_parents_are_refused_without_touching_the_target() {
    for parent in [false, true] {
        let f = Fixture::new();
        f.install("1.0.0", "kiteapp");
        let p = f.paths();
        let original = if parent {
            p.binaries.parent().unwrap().to_owned()
        } else {
            p.binaries.clone()
        };
        let target = f.home.join("saved-binaries");
        fs::rename(&original, &target).unwrap();
        symlink(&target, &original).unwrap();
        let saved_binary = if parent {
            target.join("radio/1.0.0/radio")
        } else {
            target.join("1.0.0/radio")
        };
        let before = fs::read(&saved_binary).unwrap();
        error(
            f.installer("2.0.0").install(&f.args("gardenapp"), 2),
            1,
            FILES,
        );
        assert_eq!(
            f.installer("1.0.0").uninstall("kiteapp").unwrap_err(),
            Failure::new(1, FILES)
        );
        assert_eq!(fs::read(&saved_binary).unwrap(), before);
        assert!(p.clients.join("kiteapp.json").exists());
    }
}

#[test]
fn leftover_owned_temps_are_removed_but_other_files_and_links_stay() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let p = f.paths();
    let temps = [
        p.state.join(".install.json.123.tmp"),
        p.clients.join(".kiteapp.json.123.tmp"),
        p.agent
            .parent()
            .unwrap()
            .join(".ai.awna.radio.plist.123.tmp"),
        p.binary("1.0.0").parent().unwrap().join("radio.123.tmp"),
    ];
    for path in &temps {
        fs::write(path, b"stale").unwrap();
    }
    let other = p.state.join(".install.json.abc.tmp");
    fs::write(&other, b"keep").unwrap();
    let linked = p.state.join(".install.json.124.tmp");
    symlink(&other, &linked).unwrap();
    f.install("1.0.0", "gardenapp");
    for path in &temps {
        assert!(!path.exists(), "leftover temp remains: {}", path.display());
    }
    assert_eq!(fs::read(&other).unwrap(), b"keep");
    assert!(fs::symlink_metadata(&linked).unwrap().is_symlink());
    // Uninstall performs the same cleanup after taking the lock.
    fs::write(&temps[0], b"stale").unwrap();
    f.installer("1.0.0").uninstall("gardenapp").unwrap();
    assert!(!temps[0].exists());
}

#[test]
fn a_hard_link_to_the_own_store_is_refused() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let p = f.paths();
    drop(radio_rooms::RoomStore::open(&p.rooms).unwrap());
    let alias = f.home.join("rooms-alias.db");
    fs::hard_link(&p.rooms, &alias).unwrap();
    let mut args = f.args("gardenapp");
    args.rooms = Some(alias);
    error(
        f.installer("1.0.0").install(&args, 2),
        2,
        "the store to import is the Radio's own",
    );
    assert!(!p.clients.join("gardenapp.json").exists());
}

#[test]
fn a_failed_snapshot_never_bootstraps_the_new_version() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    fs::write(f.paths().rooms, b"not a store").unwrap();
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(|| panic!("started without a snapshot")),
    );
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed; the version before it is running again",
    );
    assert_eq!(fs::read(f.paths().rooms).unwrap(), b"not a store");
    assert!(!f.paths().binary("2.0.0").exists());
    assert!(!f.paths().state.join("rooms.db.before-upgrade").exists());
    assert_installed(&f, "1.0.0");
}

#[test]
fn a_successful_upgrade_replaces_a_stale_snapshot_and_removes_it() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    drop(radio_rooms::RoomStore::open(&f.paths().rooms).unwrap());
    let snapshot = f.paths().state.join("rooms.db.before-upgrade");
    fs::write(&snapshot, b"stale").unwrap();
    let saved = snapshot.clone();
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(move || {
            assert!(fs::read(&saved).unwrap().starts_with(b"SQLite format 3"));
        }),
    );
    f.install("2.0.0", "gardenapp");
    assert!(!snapshot.exists());
    assert_installed(&f, "2.0.0");
}

#[test]
fn rollback_refuses_a_binary_parent_linked_during_bootstrap() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let binaries = f.paths().binaries;
    let target = f.home.join("moved-binaries");
    let saved = target.clone();
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(move || {
            fs::rename(&binaries, &target).unwrap();
            symlink(&target, &binaries).unwrap();
        }),
    );
    f.quiet(&["2.0.0"]);
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        FILES,
    );
    assert!(saved.join("2.0.0/radio").exists());
    assert!(saved.join("1.0.0/radio").exists());
}

#[test]
fn rollback_keeps_the_new_binary_when_it_cannot_stop_the_daemon() {
    let f = Fixture::new();
    f.install("1.0.0", "kiteapp");
    let state = Arc::downgrade(&f.fake.state);
    f.fake.state.lock().unwrap().on_bootstrap.insert(
        "2.0.0".into(),
        Arc::new(move || {
            state.upgrade().unwrap().lock().unwrap().stuck = true;
        }),
    );
    f.quiet(&["2.0.0"]);
    error(
        f.installer("2.0.0").install(&f.args("gardenapp"), 2),
        1,
        "the upgrade failed, and the version before it did not start again",
    );
    assert!(f.paths().binary("2.0.0").exists());
    assert_eq!(
        f.fake.state.lock().unwrap().loaded,
        Some(f.paths().binary("2.0.0"))
    );
}
