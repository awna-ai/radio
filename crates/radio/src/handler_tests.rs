use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    dir: PathBuf,
    handler: Handler,
    key: EndpointId,
}
impl Fixture {
    fn new(script: &str) -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "radio-handler-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("handler");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("notes.json"), serde_json::to_vec(&serde_json::json!({"app":"notes","app_version":"1.0.0","majors":[1],"handler":path,"at":0})).unwrap()).unwrap();
        let audit = Log::open(&dir.join("audit.log")).unwrap();
        let handler = Handler::new(
            Some(dir.clone()),
            dir.clone(),
            audit,
            Duration::from_secs(3),
        );
        Self {
            dir,
            handler,
            key: radio_link::SecretKey::from_bytes(&[7; 32]).public(),
        }
    }
    fn run(&self, body: &str) -> Ran {
        self.handler.run(&Envelope {
            app: "notes",
            from: self.key,
            from_name: Some("birch"),
            id: "0123456789abcdef0123456789abcdef",
            body,
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_record_outside_the_app_name_rules_is_never_read() {
    let f = Fixture::new("/bin/cat");
    let sub = f.dir.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        sub.join("notes.json"),
        serde_json::to_vec(&serde_json::json!({
            "app": "sub/notes",
            "app_version": "1.0.0",
            "majors": [1],
            "handler": f.dir.join("handler"),
            "at": 0
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(install::recorded_handler(&f.dir, "sub/notes").is_none());
    assert!(!f.handler.has_handler("sub/notes"));
}

#[test]
fn recorded_handler_gets_message_argument_and_body() {
    let f = Fixture::new("printf '%s:' \"$1\"; /bin/cat");
    assert!(f.handler.has_handler("notes"));
    assert!(matches!(f.run("hello"), Ran::Reply { reply, exit: 0 } if reply == "message:hello"));
}

#[test]
fn bodies_naming_paths_still_run_only_the_recorded_handler() {
    for body in ["/bin/sh", "../../x"] {
        let f = Fixture::new("printf ran > \"$HOME/marker\"; printf '%s:' \"$1\"; /bin/cat");
        assert!(
            matches!(f.run(body), Ran::Reply { reply, exit: 0 } if reply == format!("message:{body}"))
        );
        assert_eq!(fs::read_to_string(f.dir.join("marker")).unwrap(), "ran");
    }
}

#[test]
fn child_has_only_the_listed_environment_and_root_working_folder() {
    // The shell recreates SHLVL on exec even after unset; exclude that shell addition.
    let f = Fixture::new("unset PWD SHLVL _; exec /usr/bin/env -u SHLVL");
    let Ran::Reply { reply, exit: 0 } = f.run("hello") else {
        panic!("no reply");
    };
    let actual: std::collections::BTreeMap<_, _> =
        reply.lines().map(|s| s.split_once('=').unwrap()).collect();
    assert_eq!(
        actual.len(),
        5,
        "environment names: {:?}",
        actual.keys().collect::<Vec<_>>()
    );
    assert_eq!(actual["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin");
    assert_eq!(actual["HOME"], f.dir.to_str().unwrap());
    assert_eq!(actual["RADIO_FROM_KEY"], f.key.to_string());
    assert_eq!(actual["RADIO_FROM_NAME"], "birch");
    assert_eq!(
        actual["RADIO_MESSAGE_ID"],
        "0123456789abcdef0123456789abcdef"
    );
    let f = Fixture::new("/bin/pwd");
    assert!(matches!(f.run("hello"), Ran::Reply { reply, exit: 0 } if reply == "/\n"));
}

#[test]
fn missing_name_uses_key_and_missing_record_never_runs() {
    let f = Fixture::new("printf '%s' \"$RADIO_FROM_NAME\"");
    let result = f.handler.run(&Envelope {
        app: "notes",
        from: f.key,
        from_name: None,
        id: "0123456789abcdef0123456789abcdef",
        body: "hello",
    });
    assert!(matches!(result, Ran::Reply {reply, exit: 0} if reply == f.key.to_string()));
    fs::remove_file(f.dir.join("notes.json")).unwrap();
    assert!(!f.handler.has_handler("notes"));
    assert!(matches!(f.run("hello"), Ran::NoHandler));
}

#[test]
fn replies_reject_excess_lines_controls_and_invalid_utf8() {
    for script in [
        "printf '1\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n'",
        "printf '\\001'",
        "printf '\\177'",
        "printf '\\377'",
    ] {
        let f = Fixture::new(script);
        assert!(matches!(f.run("hello"), Ran::BadReply), "{script}");
    }
    let f = Fixture::new("printf '1\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n'");
    assert!(matches!(f.run("hello"), Ran::Reply { exit: 0, .. }));
    let f = Fixture::new("printf 'a\\tb\\n'");
    assert!(matches!(f.run("hello"), Ran::Reply { exit: 0, .. }));
}

#[test]
fn oversized_reply_kills_the_handler_promptly() {
    let f =
        Fixture::new("echo $$ > \"$HOME/parent\"; /usr/bin/head -c 4097 /dev/zero; /bin/sleep 30");
    let start = Instant::now();
    assert!(matches!(f.run("hello"), Ran::BadReply));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_gone(read_pid(&f.dir.join("parent")));
    let f = Fixture::new("/usr/bin/head -c 4096 /dev/zero | /usr/bin/tr '\\000' x");
    assert!(matches!(f.run("hello"), Ran::Reply {reply,exit: 0} if reply.len() == 4096));
}

fn read_pid(path: &std::path::Path) -> libc::pid_t {
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}
fn assert_gone(pid: libc::pid_t) {
    let start = Instant::now();
    loop {
        // SAFETY: signal zero probes a pid without sending a signal.
        if unsafe { libc::kill(pid, 0) } == -1 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "process {pid} survived"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn deadline_kills_the_handler_and_its_child() {
    let mut f =
        Fixture::new("echo $$ > \"$HOME/parent\"; /bin/sleep 30 & echo $! > \"$HOME/child\"; wait");
    f.handler.timeout = Duration::from_millis(250);
    let start = Instant::now();
    assert!(matches!(f.run("hello"), Ran::Timeout));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_gone(read_pid(&f.dir.join("parent")));
    assert_gone(read_pid(&f.dir.join("child")));
}

#[test]
fn deadline_kills_a_handler_that_left_its_group() {
    let mut f = Fixture::new(
        "exec /usr/bin/perl -e 'setpgrp(0, getpgrp(getppid())) or exit 9; open my $f, \">\", \"$ENV{HOME}/parent\" or exit 8; print $f $$; close $f; sleep 30'",
    );
    f.handler.timeout = Duration::from_secs(1);
    let start = Instant::now();
    assert!(matches!(f.run("hello"), Ran::Timeout));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_gone(read_pid(&f.dir.join("parent")));
}

#[test]
fn completed_handler_kills_its_background_child() {
    assert_completed_child_killed(
        "/bin/sleep 30 >/dev/null 2>&1 & echo $! > \"$HOME/child\"; printf done",
    );
}

#[test]
fn completed_handler_kills_a_child_holding_its_output_pipe() {
    assert_completed_child_killed("/bin/sleep 30 & echo $! > \"$HOME/child\"; printf done");
}

fn assert_completed_child_killed(script: &str) {
    struct Cleanup(Option<libc::pid_t>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                // SAFETY: this pid belongs to the disposable sleep started below.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    let f = Fixture::new(script);
    let start = Instant::now();
    let result = f.run("hello");
    let pid = read_pid(&f.dir.join("child"));
    let mut child = Cleanup(Some(pid));
    assert!(matches!(result, Ran::Reply { reply, exit: 0 } if reply == "done"));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_gone(pid);
    child.0 = None;
}

#[test]
fn reply_rejects_right_to_left_override() {
    assert_rejected_character('\u{202e}');
}

#[test]
fn reply_rejects_left_to_right_isolate() {
    assert_rejected_character('\u{2066}');
}

#[test]
fn reply_rejects_zero_width_space() {
    assert_rejected_character('\u{200b}');
}

#[test]
fn reply_rejects_byte_order_mark() {
    assert_rejected_character('\u{feff}');
}

fn assert_rejected_character(character: char) {
    let f = Fixture::new(&format!("printf '{character}'"));
    assert!(
        matches!(f.run("hello"), Ran::BadReply),
        "U+{:04X}",
        character as u32
    );
}

#[test]
fn exit_codes_and_signals_are_reported() {
    let f = Fixture::new("exit 3");
    assert!(matches!(f.run("hello"), Ran::Reply { exit: 3, .. }));
    let f = Fixture::new("kill -TERM $$");
    assert!(matches!(f.run("hello"), Ran::Reply { exit: 143, .. }));
}

#[test]
fn changed_permissions_links_and_unsafe_folders_never_run() {
    for change in 0..3 {
        let f = Fixture::new("echo ran > \"$HOME/marker\"");
        let path = f.dir.join("handler");
        match change {
            0 => fs::set_permissions(&path, fs::Permissions::from_mode(0o720)).unwrap(),
            1 => {
                fs::rename(&path, f.dir.join("real")).unwrap();
                symlink(f.dir.join("real"), &path).unwrap();
            }
            2 => fs::set_permissions(&f.dir, fs::Permissions::from_mode(0o707)).unwrap(),
            _ => unreachable!(),
        }
        assert!(!f.handler.has_handler("notes"));
        assert!(matches!(f.run("hello"), Ran::NoHandler));
        assert!(!f.dir.join("marker").exists());
    }
}

#[test]
fn audit_is_private_escaped_and_contains_only_refusal_metadata() {
    let f = Fixture::new("printf reply-marker");
    let _ = f.run("body-marker");
    f.handler
        .refused(&f.key, Some("birch\ncedar"), "notes", "busy");
    let path = f.dir.join("audit.log");
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(" refused "));
    assert!(text.contains(&f.key.to_string()));
    assert!(text.contains("name=\"birch\\ncedar\""));
    assert!(text.contains("app=\"notes\" reason=\"busy\""));
    for absent in [
        "body-marker",
        "reply-marker",
        "0123456789abcdef0123456789abcdef",
    ] {
        assert!(!text.contains(absent));
    }
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn audit_drops_an_invalid_app_name() {
    let f = Fixture::new("exit 0");
    f.handler
        .refused(&f.key, None, &"x".repeat(60_000), "not_for_us");
    let text = fs::read_to_string(f.dir.join("audit.log")).unwrap();
    assert!(text.len() < 300, "audit line has {} bytes", text.len());
    assert!(text.contains("app=\"\""));
}

#[test]
fn audit_uses_the_existing_one_mebibyte_rotation() {
    let f = Fixture::new("exit 0");
    let path = f.dir.join("audit.log");
    fs::write(&path, vec![b'x'; crate::log::LOG_MAX_BYTES as usize]).unwrap();
    f.handler.refused(&f.key, None, "notes", "clock");
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(
        text.lines()
            .next()
            .unwrap()
            .ends_with("log truncated limit=1048576")
    );
    assert!(text.contains("reason=\"clock\""));
}

#[test]
fn an_elapsed_deadline_cannot_return_a_completed_reply() {
    let mut f = Fixture::new("exit 0");
    f.handler.timeout = Duration::ZERO;
    assert!(matches!(f.run("hello"), Ran::Timeout));
}
