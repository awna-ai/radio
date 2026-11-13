use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let mut random = [0u8; 16];
        use std::io::Read;
        File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut random)
            .unwrap();
        let dir =
            std::env::temp_dir().join(format!("radio-log-{:032x}", u128::from_ne_bytes(random)));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        Self(dir)
    }
    fn file(&self) -> File {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.0.join("log"))
            .unwrap()
    }
    fn read(&self) -> String {
        fs::read_to_string(self.0.join("log")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn utc_epoch() {
    assert_eq!(utc(0), "1970-01-01T00:00:00Z");
}
#[test]
fn utc_leap_day() {
    assert_eq!(utc(951782400), "2000-02-29T00:00:00Z");
}
#[test]
fn utc_current_century() {
    assert_eq!(utc(1794539296), "2026-11-13T03:08:16Z");
}
#[test]
fn utc_century_end() {
    assert_eq!(utc(4102444799), "2099-12-31T23:59:59Z");
}
#[test]
fn line_format() {
    assert_eq!(
        line(
            0,
            "serve",
            "failed",
            &[("op", Field::Text("read")), ("n", Field::Number(3))]
        ),
        "1970-01-01T00:00:00Z serve failed op=\"read\" n=3\n"
    );
}
#[test]
fn text_is_escaped_on_one_line() {
    let text = line(
        0,
        "serve",
        "failed",
        &[("error", Field::Text("\"\n\u{202e}"))],
    );
    assert_eq!(
        text,
        "1970-01-01T00:00:00Z serve failed error=\"\\\"\\n\\u{202e}\"\n"
    );
    assert_eq!(text.bytes().filter(|b| *b == b'\n').count(), 1);
    assert!(text.ends_with('\n'));
}
#[test]
fn regular_file_is_truncated_at_cap() {
    let fixture = Fixture::new();
    let log = Log::to_file(fixture.file(), 200);
    for _ in 0..7 {
        log.event("serve", "bound", &[]);
    }
    assert!(fixture.read().len() >= 200);
    log.event("serve", "bound", &[]);
    let text = fixture.read();
    assert!(text.len() < 200 + "1970-01-01T00:00:00Z serve bound\n".len());
    assert!(
        text.lines()
            .next()
            .unwrap()
            .ends_with("log truncated limit=200")
    );
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn append_does_not_create_a_missing_file() {
    let f = Fixture::new();
    append(&f.0.join("log"), "install", "installed", &[]);
    assert!(!f.0.join("log").exists());
}

#[test]
fn append_does_not_follow_a_symlink() {
    let f = Fixture::new();
    fs::write(f.0.join("target"), "kept\n").unwrap();
    std::os::unix::fs::symlink(f.0.join("target"), f.0.join("log")).unwrap();
    append(&f.0.join("log"), "install", "installed", &[]);
    assert_eq!(fs::read_to_string(f.0.join("target")).unwrap(), "kept\n");
}

#[test]
fn append_skips_a_directory() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("log")).unwrap();
    append(&f.0.join("log"), "install", "installed", &[]);
    assert!(f.0.join("log").is_dir());
    assert_eq!(fs::read_dir(f.0.join("log")).unwrap().count(), 0);
}

#[test]
fn append_writes_one_line_without_truncating() {
    let f = Fixture::new();
    fs::write(f.0.join("log"), "kept\n").unwrap();
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    append(
        &f.0.join("log"),
        "install",
        "installed",
        &[("version", Field::Text("1.0.0"))],
    );
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let text = f.read();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "kept");
    let (time, event) = lines[1].split_once(' ').unwrap();
    assert!((before..=after).any(|secs| utc(secs) == time));
    assert_eq!(event, "install installed version=\"1.0.0\"");
    assert!(text.ends_with('\n'));
}

#[test]
fn stderr_caps_the_log_inode() {
    let f = Fixture::new();
    let file = f.file();
    file.set_len(LOG_MAX_BYTES + 1).unwrap();
    let alias = f.0.join("alias");
    fs::hard_link(f.0.join("log"), &alias).unwrap();
    let log = Log::stderr_file(file, &alias);
    log.event("serve", "bound", &[]);
    let text = f.read();
    assert_eq!(text.lines().count(), 2);
    assert!(
        text.lines()
            .next()
            .unwrap()
            .ends_with("log truncated limit=1048576")
    );
    assert!(text.lines().last().unwrap().ends_with("serve bound"));
}

#[test]
fn stderr_preserves_another_file_past_the_log_cap() {
    let f = Fixture::new();
    let file = f.file();
    let before = LOG_MAX_BYTES + 1;
    file.set_len(before).unwrap();
    fs::write(f.0.join("radio.log"), "kept\n").unwrap();
    let log = Log::stderr_file(file, &f.0.join("radio.log"));
    log.event("serve", "bound", &[]);
    assert!(fs::metadata(f.0.join("log")).unwrap().len() >= before);
    assert_eq!(fs::read_to_string(f.0.join("radio.log")).unwrap(), "kept\n");
}

impl Fixture {
    fn fifo(&self) -> PathBuf {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = self.0.join("fifo");
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        path
    }
}

#[test]
fn append_writes_nothing_to_a_fifo_with_a_reader() {
    use std::io::Read;
    let f = Fixture::new();
    let path = f.fifo();
    let mut reader = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    append(&path, "install", "installed", &[]);
    let mut bytes = [0; 256];
    let read = reader.read(&mut bytes);
    assert!(
        matches!(read, Ok(0))
            || matches!(read, Err(ref error) if error.kind() == io::ErrorKind::WouldBlock),
        "append wrote to a FIFO: {read:?}"
    );
}

#[test]
fn append_returns_without_a_fifo_reader() {
    use std::{sync::mpsc, thread, time::Duration};
    let f = Fixture::new();
    let path = f.fifo();
    let target = path.clone();
    let (done, completion) = mpsc::channel();
    let worker = thread::spawn(move || {
        append(&target, "install", "installed", &[]);
        done.send(()).unwrap();
    });
    let result = completion.recv_timeout(Duration::from_secs(1));
    // Release a blocking open before asserting, so a failing mutation leaves no worker behind.
    let _reader = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .unwrap();
    worker.join().unwrap();
    assert!(result.is_ok(), "append blocked without a FIFO reader");
}
