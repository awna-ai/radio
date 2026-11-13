use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn body(node: &iroh::SecretKey) -> ComputerBody {
    ComputerBody {
        fleet: "ab".repeat(32),
        key: node.public().to_string(),
        version: 1,
        name: "garden".into(),
        roles: vec!["hub".into()],
        addrs: vec!["127.0.0.1:7000".parse().unwrap()],
        apps: BTreeMap::from([("lantern".into(), "{\"x\":1}".into())]),
    }
}

#[test]
fn signed_record_verifies_and_round_trips() {
    let node = iroh::SecretKey::generate();
    let record = sign(body(&node), &node);
    assert_eq!(record.verify(), Ok(()));
    assert_eq!(
        serde_json::from_slice::<ComputerRecord>(&serde_json::to_vec(&record).unwrap()).unwrap(),
        record
    );
    assert!(valid_role("hub"));
    assert!(!valid_role("Hub"));
}

#[test]
fn signatures_bind_the_computers_key_every_field_and_domain() {
    let node = iroh::SecretKey::generate();
    let base = body(&node);
    let mut records = vec![sign(base.clone(), &iroh::SecretKey::generate())];
    for (field, value) in [
        ("apps", json!({"lantern": "changed"})),
        ("version", json!(2)),
        ("roles", json!(["lander"])),
        ("addrs", json!(["127.0.0.1:7001"])),
        ("fleet", json!("cd".repeat(32))),
        ("name", json!("birch")),
        (
            "key",
            json!(iroh::SecretKey::generate().public().to_string()),
        ),
    ] {
        let mut record = serde_json::to_value(sign(base.clone(), &node)).unwrap();
        record["body"][field] = value;
        records.push(serde_json::from_value(record).unwrap());
    }
    let mut raw = sign(base.clone(), &node);
    raw.signature =
        data_encoding::HEXLOWER.encode(&node.sign(&serde_json::to_vec(&base).unwrap()).to_bytes());
    records.push(raw);
    for record in records {
        assert_eq!(
            record.verify(),
            Err("the computer record's signature does not verify")
        );
    }
}

#[test]
fn malformed_fields_are_refused() {
    let node = iroh::SecretKey::generate();
    let base = serde_json::to_value(body(&node)).unwrap();
    let mut many_apps = BTreeMap::new();
    for i in 0..=APPS_MAX {
        many_apps.insert(format!("lantern{i}"), "text");
    }
    for (field, value) in [
        ("fleet", json!("AB".repeat(32))),
        ("key", json!("a".repeat(63))),
        ("version", json!(0)),
        ("version", json!(i64::MAX as u64 + 1)),
        ("name", json!("Garden")),
        ("roles", json!(["Hub"])),
        ("roles", json!(["lander", "hub"])),
        ("roles", json!(["hub", "hub"])),
        (
            "roles",
            json!(
                (0..=ROLES_MAX)
                    .map(|i| format!("hub{i}"))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            "addrs",
            json!(["127.0.0.1:7000", "[::1]:7000", "127.0.0.2:7000"]),
        ),
        ("addrs", json!(["127.0.0.1:7000", "127.0.0.2:7000"])),
        ("apps", json!(many_apps)),
        ("apps", json!({"Lantern": "text"})),
        ("apps", json!({"lantern": ""})),
        ("apps", json!({"lantern": "x".repeat(SECTION_MAX + 1)})),
    ] {
        let mut changed = base.clone();
        changed[field] = value;
        assert_eq!(
            sign(serde_json::from_value(changed).unwrap(), &node).verify(),
            Err("not a computer record"),
            "{field}"
        );
    }
    for signature in ["f".repeat(127), "F".repeat(128), "z".repeat(128)] {
        let mut record = sign(body(&node), &node);
        record.signature = signature;
        assert_eq!(record.verify(), Err("not a computer record"));
    }
}

#[test]
fn escaped_sections_respect_the_serialized_record_limit() {
    let node = iroh::SecretKey::generate();
    let mut body = body(&node);
    body.apps = (0..APPS_MAX)
        .map(|i| (format!("lantern{i}"), "\u{1}".repeat(SECTION_MAX)))
        .collect();
    let record = sign(body, &node);
    assert!(serde_json::to_vec(&record).unwrap().len() > COMPUTER_RECORD_MAX);
    assert_eq!(record.verify(), Err("the computer record is too large"));
}

#[test]
fn unknown_fields_are_refused() {
    let node = iroh::SecretKey::generate();
    let base = serde_json::to_value(sign(body(&node), &node)).unwrap();
    let mut record = base.clone();
    record["extra"] = json!(true);
    assert!(serde_json::from_value::<ComputerRecord>(record).is_err());
    let mut record = base;
    record["body"]["extra"] = json!(true);
    assert!(serde_json::from_value::<ComputerRecord>(record).is_err());
}
