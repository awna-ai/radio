use super::{BodySource, Command, parse};
use radio_client::{Member, REQUEST_MAX_BYTES, Request};
use std::{
    fs,
    io::{self, Cursor, Read},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).into()).collect()
}

fn member(kind: &str, name: &str) -> Member {
    Member {
        kind: kind.into(),
        name: name.into(),
    }
}

fn post() -> Request {
    Request::Post {
        room: "garden".into(),
        author: member("helper", "juniper"),
        body: String::new(),
    }
}

#[test]
fn valid_commands_preserve_requests_and_body_sources() {
    let cases = vec![
        (vec!["hello"], Request::Hello {}, None),
        (
            vec!["register", "--app", "lantern", "--app-version", "1.0.0"],
            Request::Register {
                app: "lantern".into(),
                app_version: "1.0.0".into(),
                members: vec![],
            },
            None,
        ),
        (
            vec![
                "register",
                "--app-version",
                "1.0.0",
                "--member",
                "helper:juniper",
                "--app",
                "lantern",
                "--member",
                "human:sam:scribe",
            ],
            Request::Register {
                app: "lantern".into(),
                app_version: "1.0.0".into(),
                members: vec![member("helper", "juniper"), member("human", "sam:scribe")],
            },
            None,
        ),
        (
            vec!["room", "create", "garden", "--title", "Garden"],
            Request::CreateRoom {
                room: "garden".into(),
                title: "Garden".into(),
            },
            None,
        ),
        (
            vec!["rooms"],
            Request::ListRooms {
                after: None,
                limit: None,
            },
            None,
        ),
        (
            vec!["rooms", "--after", "garden", "--limit", "2"],
            Request::ListRooms {
                after: Some("garden".into()),
                limit: Some(2),
            },
            None,
        ),
        (
            vec!["rooms", "--limit", "0", "--after", "--garden"],
            Request::ListRooms {
                after: Some("--garden".into()),
                limit: Some(0),
            },
            None,
        ),
        (
            vec!["rooms", "--limit", "501"],
            Request::ListRooms {
                after: None,
                limit: Some(501),
            },
            None,
        ),
        (
            vec!["rooms", "--limit", "4294967295"],
            Request::ListRooms {
                after: None,
                limit: Some(u32::MAX),
            },
            None,
        ),
        (
            vec![
                "post",
                "garden",
                "--as",
                "helper:juniper",
                "--body",
                "Hello garden",
            ],
            post(),
            Some(BodySource::Text("Hello garden".into())),
        ),
        (
            vec![
                "post",
                "garden",
                "--body",
                "--hello",
                "--as",
                "helper:juniper",
            ],
            post(),
            Some(BodySource::Text("--hello".into())),
        ),
        (
            vec![
                "post",
                "garden",
                "--as",
                "helper:juniper",
                "--body-file",
                "draft.txt",
            ],
            post(),
            Some(BodySource::File(PathBuf::from("draft.txt"))),
        ),
        (
            vec![
                "post",
                "garden",
                "--body-file",
                "-",
                "--as",
                "helper:juniper",
            ],
            post(),
            Some(BodySource::File(PathBuf::from("-"))),
        ),
        (
            vec!["read", "garden"],
            Request::Read {
                room: "garden".into(),
                since: None,
                limit: None,
            },
            None,
        ),
        (
            vec!["read", "garden", "--since", "2", "--limit", "1"],
            Request::Read {
                room: "garden".into(),
                since: Some(2),
                limit: Some(1),
            },
            None,
        ),
        (
            vec![
                "read",
                "garden",
                "--limit",
                "501",
                "--since",
                "18446744073709551615",
            ],
            Request::Read {
                room: "garden".into(),
                since: Some(u64::MAX),
                limit: Some(501),
            },
            None,
        ),
        (
            vec!["read", "garden", "--since", "0"],
            Request::Read {
                room: "garden".into(),
                since: Some(0),
                limit: None,
            },
            None,
        ),
        (
            vec!["line", "list", "garden"],
            Request::Status {
                room: "garden".into(),
            },
            None,
        ),
        (
            vec![
                "line",
                "set",
                "garden",
                "mood",
                "--Ready",
                "--as",
                "helper:juniper",
            ],
            Request::StatusSet {
                room: "garden".into(),
                key: "mood".into(),
                line: "--Ready".into(),
                author: member("helper", "juniper"),
                expires_at: None,
            },
            None,
        ),
        (
            vec![
                "line",
                "set",
                "garden",
                "mood",
                "Ready",
                "--as",
                "human:sam",
            ],
            Request::StatusSet {
                room: "garden".into(),
                key: "mood".into(),
                line: "Ready".into(),
                author: member("human", "sam"),
                expires_at: None,
            },
            None,
        ),
        (
            vec![
                "line",
                "set",
                "garden",
                "mood",
                "Ready",
                "--as",
                "human:sam",
                "--expires-at",
                "4102444800",
            ],
            Request::StatusSet {
                room: "garden".into(),
                key: "mood".into(),
                line: "Ready".into(),
                author: member("human", "sam"),
                expires_at: Some(4102444800.0),
            },
            None,
        ),
        (
            vec![
                "line",
                "set",
                "garden",
                "mood",
                "Ready",
                "--expires-at",
                "-1.5e2",
                "--as",
                "helper:juniper:scribe",
            ],
            Request::StatusSet {
                room: "garden".into(),
                key: "mood".into(),
                line: "Ready".into(),
                author: member("helper", "juniper:scribe"),
                expires_at: Some(-150.0),
            },
            None,
        ),
        (
            vec!["line", "clear", "garden", "mood", "--as", "helper:juniper"],
            Request::StatusClear {
                room: "garden".into(),
                key: "mood".into(),
                author: member("helper", "juniper"),
            },
            None,
        ),
    ];
    for (words, request, body) in cases {
        assert_eq!(
            parse(&args(&words)),
            Ok(Command { request, body }),
            "{words:?}"
        );
    }
}

#[test]
fn invalid_commands_are_usage_errors() {
    let cases: &[&[&str]] = &[
        &[],
        &["unknown"],
        &["room"],
        &["room", "delete", "garden"],
        &["line"],
        &["line", "drop", "garden"],
        &["hello", "extra"],
        &["room", "create"],
        &["room", "create", "--garden", "--title", "Garden"],
        &["room", "create", "garden", "extra", "--title", "Garden"],
        &["room", "create", "garden", "--title"],
        &["room", "create", "garden", "--unknown", "Garden"],
        &[
            "room", "create", "garden", "--title", "Garden", "--title", "Changed",
        ],
        &["room", "create", "garden"],
        &["room", "create", "--title", "Garden", "garden"],
        &["register", "--app", "lantern"],
        &["register", "--app-version", "1.0.0"],
        &[
            "register",
            "--app",
            "lantern",
            "--app",
            "lantern",
            "--app-version",
            "1.0.0",
        ],
        &[
            "register",
            "--app",
            "lantern",
            "--app-version",
            "1.0.0",
            "--app-version",
            "1.0.0",
        ],
        &["post", "garden", "--as", "helper:juniper"],
        &["post", "garden", "--body", "hello"],
        &[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body",
            "hello",
            "--body-file",
            "draft.txt",
        ],
        &[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body",
            "hello",
            "--body",
            "hello",
        ],
        &[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--body-file",
            "draft.txt",
            "--body-file",
            "draft.txt",
        ],
        &[
            "post",
            "garden",
            "--as",
            "helper:juniper",
            "--as",
            "helper:juniper",
            "--body",
            "hello",
        ],
        &["read"],
        &["read", "--garden"],
        &["read", "garden", "extra"],
        &["line", "list"],
        &["line", "set", "garden", "mood"],
        &["line", "set", "garden", "mood", "Ready"],
        &["line", "clear", "garden", "mood"],
        &["rooms", "--limit", "1", "--limit", "2"],
        &["rooms", "--after", "garden", "--after", "garden"],
        &["read", "garden", "--since", "1", "--since", "2"],
        &[
            "line",
            "set",
            "garden",
            "mood",
            "Ready",
            "--as",
            "helper:juniper",
            "--expires-at",
            "1",
            "--expires-at",
            "2",
        ],
    ];
    for words in cases {
        assert_eq!(parse(&args(words)), Err(()), "{words:?}");
    }
    for value in ["", "-1", "+1", "1.0", " 1", "1 ", "١", "4294967296"] {
        for mut words in [vec!["rooms"], vec!["read", "garden"]] {
            words.extend(["--limit", value]);
            assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
        }
    }
    for value in [
        "",
        "-1",
        "+1",
        "1.0",
        " 1",
        "1 ",
        "١",
        "18446744073709551616",
    ] {
        let words = ["read", "garden", "--since", value];
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
    for value in ["", "later", "NaN", "inf", "-inf", "1e999"] {
        let words = [
            "line",
            "set",
            "garden",
            "mood",
            "Ready",
            "--as",
            "helper:juniper",
            "--expires-at",
            value,
        ];
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
    for value in ["helper", "Helper:juniper", ":juniper", "helper:"] {
        for mut words in [
            vec![
                "register",
                "--app",
                "lantern",
                "--app-version",
                "1.0.0",
                "--member",
            ],
            vec!["post", "garden", "--body", "hello", "--as"],
            vec!["line", "set", "garden", "mood", "Ready", "--as"],
            vec!["line", "clear", "garden", "mood", "--as"],
        ] {
            words.push(value);
            assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
        }
    }
    for mut words in [
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
            "hello",
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
        words.extend(["--v", "2"]);
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
}

struct BodyFile(PathBuf);

impl BodyFile {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(format!(
            "/tmp/rd-body-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes).unwrap();
        Self(path)
    }
}

impl Drop for BodyFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn resolve(source: BodySource, stdin: impl Read) -> Result<Request, (i32, String)> {
    Command {
        request: post(),
        body: Some(source),
    }
    .resolve(stdin)
}

#[test]
fn body_sources_preserve_text_and_apply_the_cli_byte_bound() {
    for size in [4097, REQUEST_MAX_BYTES] {
        let text = "x".repeat(size);
        let file = BodyFile::new(text.as_bytes());
        for source in [
            BodySource::Text(text.clone()),
            BodySource::File(file.0.clone()),
            BodySource::File("-".into()),
        ] {
            assert_eq!(
                resolve(source, Cursor::new(text.as_bytes())),
                Ok(Request::Post {
                    room: "garden".into(),
                    author: member("helper", "juniper"),
                    body: text.clone(),
                })
            );
        }
    }
    let text = "x".repeat(REQUEST_MAX_BYTES + 1);
    let file = BodyFile::new(text.as_bytes());
    for source in [
        BodySource::Text(text.clone()),
        BodySource::File(file.0.clone()),
        BodySource::File("-".into()),
    ] {
        assert_eq!(
            resolve(source, Cursor::new(text.as_bytes())),
            Err((1, "radio: the message is empty or too long".into()))
        );
    }
    let text = "\n--Hello garden\r\n";
    assert_eq!(
        resolve(BodySource::Text(text.into()), io::empty()),
        Ok(Request::Post {
            room: "garden".into(),
            author: member("helper", "juniper"),
            body: text.into(),
        })
    );
}

#[test]
fn invalid_body_files_report_read_and_encoding_errors() {
    let invalid = BodyFile::new(&[0xff, b'\n']);
    assert_eq!(
        resolve(BodySource::File(invalid.0.clone()), io::empty()),
        Err((2, "radio: the body is not UTF-8".into()))
    );
    assert_eq!(
        resolve(BodySource::File("-".into()), Cursor::new([0xff, b'\n'])),
        Err((2, "radio: the body is not UTF-8".into()))
    );
    let missing = BodyFile::new(b"");
    fs::remove_file(&missing.0).unwrap();
    assert_eq!(
        resolve(BodySource::File(missing.0.clone()), io::empty()),
        Err((2, "radio: the body file could not be read".into()))
    );
    struct FailingReader;
    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    assert_eq!(
        resolve(BodySource::File("-".into()), FailingReader),
        Err((2, "radio: the body file could not be read".into()))
    );
}

#[test]
fn stdin_is_never_read_beyond_the_cli_byte_bound() {
    struct BoundedReader(usize);
    impl Read for BoundedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(self.0 > 0, "read beyond the request byte bound");
            let count = buffer.len().min(self.0);
            buffer[..count].fill(b'x');
            self.0 -= count;
            Ok(count)
        }
    }
    assert_eq!(
        resolve(
            BodySource::File("-".into()),
            BoundedReader(REQUEST_MAX_BYTES + 1)
        ),
        Err((1, "radio: the message is empty or too long".into()))
    );
}
