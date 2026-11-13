use super::{BodySource, Command, parse};
use radio_client::{Member, REQUEST_MAX_BYTES, Request};
use std::{
    fs,
    io::{self, Cursor, Read, Write},
    os::unix::fs::PermissionsExt,
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
            vec!["room", "create", "--garden", "--title", "Garden"],
            Request::CreateRoom {
                room: "--garden".into(),
                title: "Garden".into(),
            },
            None,
        ),
        (
            vec!["read", "--garden"],
            Request::Read {
                room: "--garden".into(),
                since: None,
                limit: None,
            },
            None,
        ),
        (
            vec!["line", "clear", "garden", "--k", "--as", "human:sam"],
            Request::StatusClear {
                room: "garden".into(),
                key: "--k".into(),
                author: member("human", "sam"),
            },
            None,
        ),
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
fn id_parses_to_its_request() {
    assert_eq!(
        parse(&args(&["id"])),
        Ok(Command {
            request: Request::Id {},
            body: None,
        })
    );
}

#[test]
fn id_arguments_are_usage_errors() {
    for words in [vec!["id", "extra"], vec!["id", "--v", "2"]] {
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
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
        &[
            "line",
            "set",
            "garden",
            "--expires-at",
            "1700000000",
            "--as",
            "human:sam",
        ],
        &["line", "clear", "--as", "human:sam", "--as", "human:sam"],
        &["room", "create", "--title", "--title", "Garden"],
        &[
            "line",
            "set",
            "garden",
            "--as",
            "human:sam",
            "--as",
            "human:sam",
        ],
        &["hello", "extra"],
        &["room", "create"],
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

#[test]
fn body_files_have_a_fresh_private_directory_and_remove_it_on_drop() {
    let file = BodyFile::new(b"body");
    let directory = file.0.parent().unwrap().to_path_buf();
    assert!(
        directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("rd-body-")
    );
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(&file.0).unwrap(), b"body");
    drop(file);
    assert!(!directory.exists());
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
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let file = path.join("body");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Self(file)
    }
}

impl Drop for BodyFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
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
    let missing_path = missing.0.parent().unwrap().join("missing");
    assert_eq!(
        resolve(BodySource::File(missing_path), io::empty()),
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

fn fleet_fixture(stem: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contract/v1")
        .join(format!("{stem}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn fleet_contract_requests_are_supported() {
    assert_eq!(radio_client::PROTOCOL_MINOR, 4);
    let response = serde_json::json!({"ok":true,"fleet":{"id":"garden","name":"garden","version":1,"founder":"orchard"},"recovery_file":"/recovery.key","code":"hidden","code_id":"garden","expires_at":1800.0,"codes":[{"id":"garden","created":1.0,"expires_at":1800.0,"uses":1}]});
    let typed: radio_client::Response = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), response);
    for stem in [
        "18-fleet_init",
        "19-code_create",
        "20-code_list",
        "21-code_revoke",
        "22-code_create_refused",
    ] {
        let mut value = fleet_fixture(stem)["request"].clone();
        value.as_object_mut().unwrap().remove("v");
        assert!(serde_json::from_value::<Request>(value).is_ok(), "{stem}");
    }
}

#[test]
fn fleet_commands_match_contract_requests() {
    for (stem, words) in [
        ("18-fleet_init", vec!["fleet", "init", "garden"]),
        (
            "19-code_create",
            vec!["code", "create", "--name", "orchard", "--ttl", "10m"],
        ),
        ("20-code_list", vec!["code", "list"]),
        ("21-code_revoke", vec!["code", "revoke", "0000000000000000"]),
    ] {
        let parsed = parse(&args(&words));
        assert!(parsed.is_ok(), "{words:?}");
        let mut request = serde_json::to_value(parsed.unwrap().request).unwrap();
        request["v"] = 1.into();
        assert_eq!(request, fleet_fixture(stem)["request"]);
    }
    let parsed = parse(&args(&["code", "create", "--ttl", "2h"])).unwrap();
    assert_eq!(serde_json::to_value(parsed.request).unwrap()["ttl"], 7200);
    let parsed = parse(&args(&["code", "create"])).unwrap();
    assert_eq!(
        serde_json::to_value(parsed.request).unwrap(),
        serde_json::json!({"op":"code_create"})
    );
}

#[test]
fn fleet_values_are_sent_verbatim() {
    for (words, request) in [
        (
            vec!["fleet", "init", "--name"],
            Request::FleetInit {
                name: "--name".into(),
            },
        ),
        (
            vec!["code", "revoke", "--id"],
            Request::CodeRevoke { id: "--id".into() },
        ),
        (
            vec!["code", "create", "--name", "--ttl"],
            Request::CodeCreate {
                name: Some("--ttl".into()),
                ttl: None,
            },
        ),
    ] {
        assert_eq!(
            parse(&args(&words)),
            Ok(Command {
                request,
                body: None,
            }),
            "{words:?}"
        );
    }
}

#[test]
fn invalid_fleet_commands_are_usage_errors() {
    for words in [
        vec!["code", "revoke"],
        vec!["fleet", "init"],
        vec!["fleet", "init", "garden", "orchard"],
        vec!["code", "list", "extra"],
        vec!["code", "create", "--unknown", "orchard"],
        vec!["code", "create", "--name"],
        vec!["code", "create", "--name", "orchard", "--name", "garden"],
        vec!["code", "create", "--ttl", "1m", "--ttl", "2h"],
    ] {
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
    for ttl in [
        "10",
        "m",
        "10s",
        "-1m",
        "+1m",
        "١m",
        "18446744073709551615h",
        "18446744073709551616m",
    ] {
        assert_eq!(
            parse(&args(&["code", "create", "--ttl", ttl])),
            Err(()),
            "{ttl}"
        );
    }
}

#[test]
fn computer_commands_match_contract_requests() {
    for (stem, words) in [
        ("23-computer_list", vec!["computer", "list"]),
        (
            "24-computer_accept_unknown",
            vec!["computer", "accept", "meadow"],
        ),
        ("25-computer_block_bad", vec!["computer", "block", "Meadow"]),
        (
            "26-computer_trust_unknown",
            vec!["computer", "trust", "meadow"],
        ),
    ] {
        let parsed = parse(&args(&words));
        assert!(parsed.is_ok(), "{words:?}");
        let mut request = serde_json::to_value(parsed.unwrap().request).unwrap();
        request["v"] = 1.into();
        assert_eq!(request, fleet_fixture(stem)["request"]);
    }
}

#[test]
fn computer_accept_passes_flag_shaped_positionals_to_the_daemon() {
    assert_eq!(
        parse(&args(&["computer", "accept", "--name"]))
            .unwrap()
            .request,
        Request::ComputerAccept {
            computer: "--name".into(),
        }
    );
}

#[test]
fn invalid_computer_commands_are_usage_errors() {
    for words in [
        vec!["computer", "accept"],
        vec!["computer", "accept", "meadow", "lantern"],
        vec!["computer", "list", "extra"],
        vec!["computer", "block"],
        vec!["computer", "trust"],
    ] {
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
}

#[test]
fn computer_answers_preserve_fields_and_omit_absent_names() {
    let response = serde_json::json!({
        "ok": true,
        "computers": [{"id":"key", "state":"left", "trusted":false}],
        "computer": {"id":"key", "name":"meadow", "state":"accepted", "invited_by":"inviter", "trusted":true}
    });
    let typed: radio_client::Response = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), response);
}

#[test]
fn record_commands_match_contract_requests() {
    for (stem, words) in [
        (
            "27-resign",
            vec![
                "resign",
                "--app",
                "lantern",
                "--section",
                r#"{"level":"presence"}"#,
                "--roles",
                "hub",
            ],
        ),
        (
            "30-computer_records",
            vec!["computer", "records", "--app", "lantern"],
        ),
    ] {
        let parsed = parse(&args(&words)).unwrap().resolve(io::empty()).unwrap();
        let mut request = serde_json::to_value(parsed).unwrap();
        request["v"] = 1.into();
        assert_eq!(request, fleet_fixture(stem)["request"]);
    }
}

#[test]
fn resign_resolves_sections_and_preserves_optional_roles() {
    for (words, stdin, expected) in [
        (
            vec!["resign", "--app", "lantern", "--section-file", "-"],
            "section\n",
            serde_json::json!({"op":"resign","app":"lantern","section":"section\n"}),
        ),
        (
            vec!["resign", "--app", "lantern"],
            "ignored",
            serde_json::json!({"op":"resign","app":"lantern"}),
        ),
        (
            vec!["resign", "--app", "lantern", "--section", ""],
            "",
            serde_json::json!({"op":"resign","app":"lantern","section":""}),
        ),
        (
            vec!["resign", "--app", "lantern", "--roles", ""],
            "",
            serde_json::json!({"op":"resign","app":"lantern","roles":[]}),
        ),
        (
            vec!["resign", "--app", "lantern", "--roles", "hub, lander,,Hub"],
            "",
            serde_json::json!({"op":"resign","app":"lantern","roles":["hub"," lander","","Hub"]}),
        ),
    ] {
        let request = parse(&args(&words))
            .unwrap()
            .resolve(Cursor::new(stdin))
            .unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), expected);
    }
    let file = BodyFile::new(b"section from file\n");
    let request = parse(&args(&[
        "resign",
        "--app",
        "lantern",
        "--section-file",
        file.0.to_str().unwrap(),
    ]))
    .unwrap()
    .resolve(io::empty())
    .unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::json!({"op":"resign","app":"lantern","section":"section from file\n"})
    );
}

#[test]
fn invalid_record_commands_are_usage_errors() {
    for words in [
        vec![
            "resign",
            "--app",
            "lantern",
            "--section",
            "a",
            "--section-file",
            "-",
        ],
        vec!["resign"],
        vec!["resign", "--section", "a"],
        vec!["computer", "records"],
        vec!["resign", "--app", "lantern", "--app", "meadow"],
        vec!["resign", "--app", "lantern", "--roles"],
        vec!["computer", "records", "--app", "lantern", "extra"],
    ] {
        assert_eq!(parse(&args(&words)), Err(()), "{words:?}");
    }
}

#[test]
fn record_answers_preserve_versions_and_omit_absent_sections() {
    let response = serde_json::json!({
        "ok": true,
        "record": {"id":"key", "name":"garden", "version":1700000000000_u64, "roles":["hub"], "section":"a"},
        "records": [{"id":"key", "name":"garden", "version":1700000000001_u64, "roles":[]}]
    });
    let typed: radio_client::Response = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), response);
}

#[test]
fn resign_rejects_empty_section_files_and_stdin() {
    let file = BodyFile::new(b"");
    for path in [file.0.to_str().unwrap(), "-"] {
        let command = parse(&args(&[
            "resign",
            "--app",
            "lantern",
            "--section-file",
            path,
        ]))
        .unwrap();
        assert_eq!(
            command.resolve(io::empty()),
            Err((2, "radio: the section file is empty".into()))
        );
    }
}

#[test]
fn resign_section_files_share_the_bounded_body_reader_and_errors() {
    for (bytes, expected) in [
        (vec![0xff], (2, "radio: the body is not UTF-8")),
        (
            vec![b'x'; REQUEST_MAX_BYTES + 1],
            (1, "radio: the message is empty or too long"),
        ),
    ] {
        let file = BodyFile::new(&bytes);
        for path in ["-", file.0.to_str().unwrap()] {
            let command = parse(&args(&[
                "resign",
                "--app",
                "lantern",
                "--section-file",
                path,
            ]))
            .unwrap();
            assert_eq!(
                command.resolve(Cursor::new(&bytes)),
                Err((expected.0, expected.1.into()))
            );
        }
    }
    let file = BodyFile::new(b"");
    let missing = file.0.parent().unwrap().join("missing");
    let command = parse(&args(&[
        "resign",
        "--app",
        "lantern",
        "--section-file",
        missing.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(
        command.resolve(io::empty()),
        Err((2, "radio: the body file could not be read".into()))
    );
}
