use super::*;
use serde_json::{Value, json};

fn hello() -> Hello {
    Hello {
        protocol: Protocol {
            major: LINK_MAJOR,
            minor: LINK_MINOR,
        },
        fleet: "a".repeat(64),
        sha: "b".repeat(64),
        arch: "aarch64".into(),
        name: "garden".into(),
    }
}

#[test]
fn messages_round_trip() {
    for request in [
        Request::Hello { hello: hello() },
        Request::App {
            to: "c".repeat(64),
            app: "lantern".into(),
            body: "opaque\n{invalid JSON".into(),
        },
    ] {
        assert_eq!(
            serde_json::from_slice::<Request>(&serde_json::to_vec(&request).unwrap()).unwrap(),
            request
        );
    }
    for answer in [Answer::Hello { hello: hello() }, Answer::Ok] {
        assert_eq!(
            serde_json::from_slice::<Answer>(&serde_json::to_vec(&answer).unwrap()).unwrap(),
            answer
        );
    }
}

#[test]
fn unknown_fields_and_types_are_refused() {
    let mut request = serde_json::to_value(Request::Hello { hello: hello() }).unwrap();
    for location in [vec![], vec!["hello"], vec!["hello", "protocol"]] {
        let mut extra = request.clone();
        let mut object = &mut extra;
        for part in location {
            object = &mut object[part];
        }
        object["extra"] = json!(true);
        assert!(serde_json::from_value::<Request>(extra).is_err());
    }
    for location in [vec!["hello"], vec!["hello", "protocol"]] {
        let mut extra = serde_json::to_value(Answer::Hello { hello: hello() }).unwrap();
        let mut object = &mut extra;
        for part in location {
            object = &mut object[part];
        }
        object["extra"] = json!(true);
        assert!(serde_json::from_value::<Answer>(extra).is_err());
    }
    request["type"] = json!("unknown");
    assert!(serde_json::from_value::<Request>(request).is_err());
    for answer in [
        json!({"type":"ok","extra":true}),
        json!({"type":"hello","hello":hello(),"extra":true}),
        json!({"type":"unknown"}),
    ] {
        assert!(serde_json::from_value::<Answer>(answer).is_err());
    }
    assert!(
        serde_json::from_value::<Request>(
            json!({"type":"app","to":"c".repeat(64),"app":"lantern","body":"text","extra":true})
        )
        .is_err()
    );
}

#[test]
fn hello_fields_are_checked() {
    assert!(valid_hello(&hello()));
    let mut newer_minor = hello();
    newer_minor.protocol.minor = u32::MAX;
    assert!(valid_hello(&newer_minor));
    for (field, value) in [
        ("fleet", "A".repeat(64)),
        ("fleet", "a".repeat(63)),
        ("sha", "B".repeat(64)),
        ("sha", "b".repeat(63)),
        ("arch", "".into()),
        ("arch", "a".repeat(17)),
        ("arch", "ARM64".into()),
        ("arch", "a-b".into()),
        ("name", "Garden".into()),
        ("name", "9lives".into()),
        ("name", format!("a{}", "b".repeat(32))),
    ] {
        let mut invalid = serde_json::to_value(hello()).unwrap();
        invalid[field] = Value::String(value);
        assert!(
            !valid_hello(&serde_json::from_value(invalid).unwrap()),
            "{field}"
        );
    }
    let mut wrong_major = hello();
    wrong_major.protocol.major = 2;
    assert!(!valid_hello(&wrong_major));
    let mut longest_name = hello();
    longest_name.name = format!("a{}", "b".repeat(31));
    longest_name.arch = "a0_".repeat(5);
    assert!(valid_hello(&longest_name));
}

#[test]
fn names_and_destination_are_checked() {
    for name in ["garden", "a", "a-9"] {
        assert!(member_name(name));
    }
    for name in ["", "Garden", "9lives", "a_b", "a.b", "é", "a\n"] {
        assert!(!member_name(name));
    }
    for name in ["lantern", "9lives", "a.b_c-9"] {
        assert!(app_name(name));
    }
    assert!(app_name(&"a".repeat(64)));
    for name in [
        "",
        "Lantern",
        ".lantern",
        "-lantern",
        "_lantern",
        "a/b",
        "a b",
        "é",
        &"a".repeat(65),
    ] {
        assert!(!app_name(name));
    }
    let local = iroh::SecretKey::from_bytes(&[1; 32]).public();
    let other = iroh::SecretKey::from_bytes(&[2; 32]).public();
    assert!(valid_app(&local.to_string(), "lantern", &local));
    assert!(!valid_app(&other.to_string(), "lantern", &local));
    assert!(!valid_app(&local.to_string(), "Lantern", &local));
    assert!(!valid_app(&"A".repeat(64), "lantern", &local));
}

#[test]
fn record_request_round_trips_and_refuses_unknown_fields() {
    let key = crate::SecretKey::generate();
    let record = crate::membership::sign(
        crate::membership::RecordBody::Removal {
            fleet: "a".repeat(64),
            signer: key.public().to_string(),
            counter: 1,
            subject: key.public().to_string(),
        },
        &key,
    );
    let request = Request::Record { record };
    let mut value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<Request>(value.clone()).unwrap(),
        request
    );
    value["extra"] = json!(true);
    assert!(serde_json::from_value::<Request>(value).is_err());
    assert_eq!(LINK_MINOR, 3);
}

#[test]
fn join_request_round_trips_and_refuses_unknown_fields() {
    let request = JoinRequest {
        code: "opaque".into(),
        consent: "opaque".into(),
        name: "garden".into(),
        addrs: vec!["127.0.0.1:7000".parse().unwrap()],
    };
    let mut value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<JoinRequest>(value.clone()).unwrap(),
        request
    );
    value["extra"] = json!(true);
    assert!(serde_json::from_value::<JoinRequest>(value).is_err());
}

#[test]
fn join_request_debug_hides_code_and_consent() {
    let key = iroh::SecretKey::generate();
    let code =
        crate::code::JoinCode::new("ab".repeat(32), key.public(), crate::code::Hints::default())
            .unwrap();
    let encoded = code.encode();
    let secret = encoded.split('.').nth(3).unwrap();
    let consent = crate::membership::consent(
        &key,
        &code.fleet,
        &code.inviter.to_string(),
        "garden",
        &["127.0.0.1:7000".parse().unwrap()],
    );
    let request = JoinRequest {
        code: encoded.clone(),
        consent: consent.clone(),
        name: "garden".into(),
        addrs: vec!["127.0.0.1:7000".parse().unwrap()],
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains(&encoded));
    assert!(!debug.contains(secret));
    assert!(!debug.contains(&consent));
    assert!(debug.contains("garden"));
    assert!(debug.contains("<hidden>"));
}

#[test]
fn computer_request_round_trips_and_refuses_unknown_fields() {
    let key = crate::SecretKey::generate();
    let record = crate::computer_record::sign(
        crate::computer_record::ComputerBody {
            fleet: "a".repeat(64),
            key: key.public().to_string(),
            version: 1,
            name: "garden".into(),
            roles: vec!["hub".into()],
            addrs: vec![],
            apps: [("lantern".into(), "opaque".into())].into(),
        },
        &key,
    );
    let request = Request::Computer { record };
    let mut value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<Request>(value.clone()).unwrap(),
        request
    );
    value["extra"] = json!(true);
    assert!(serde_json::from_value::<Request>(value).is_err());
    assert_eq!(LINK_MINOR, 3);
}

#[test]
fn file_names_obey_byte_and_path_rules() {
    for name in ["report-1.final_v2.txt", &"x".repeat(128)] {
        assert!(file_name(name), "{name:?}");
    }
    for name in [
        "../x",
        "a/b",
        ".hidden",
        "",
        &"x".repeat(129),
        "a b",
        "é",
        "a:b",
        "a\n",
        "a\0",
        "a\\b",
        "a\u{7f}",
        "a\u{1f}",
    ] {
        assert!(!file_name(name), "{name:?}");
    }
}

#[test]
fn file_wire_shapes_and_minor() {
    let file =
        json!({"type":"file", "to":"c".repeat(64), "app":"notes", "name":"x", "size":67108864});
    assert!(serde_json::from_value::<Request>(file.clone()).is_ok());
    let refused = json!({"type":"refused", "reason":"busy"});
    assert!(serde_json::from_value::<Answer>(refused.clone()).is_ok());
    let request: Request = serde_json::from_value(file.clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), file);
    let answer: Answer = serde_json::from_value(refused.clone()).unwrap();
    assert_eq!(serde_json::to_value(answer).unwrap(), refused);
    assert_eq!(LINK_MINOR, 3);
}

#[test]
fn file_heads_allow_the_cap_but_refuse_one_byte_more() {
    let local = crate::SecretKey::generate().public();
    for (size, expected) in [(FILE_CAP, true), (FILE_CAP + 1, false)] {
        let head = Request::File {
            to: local.to_string(),
            app: "notes".into(),
            name: "letter".into(),
            size,
        };
        let Request::File {
            to,
            app,
            name,
            size,
        } = head
        else {
            unreachable!()
        };
        assert_eq!(
            valid_file(&to, &app, &name, size, &local),
            expected,
            "size={size}"
        );
    }
}
