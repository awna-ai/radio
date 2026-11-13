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
