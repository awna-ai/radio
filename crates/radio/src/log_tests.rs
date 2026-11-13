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
