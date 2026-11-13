use super::*;
use serde_json::json;

fn introduction(node: &iroh::SecretKey) -> RecordBody {
    RecordBody::Introduction {
        fleet: "ab".repeat(32),
        inviter: node.public().to_string(),
        counter: 1,
        newcomer: iroh::SecretKey::generate().public().to_string(),
        name: "garden".into(),
        addrs: vec!["127.0.0.1:7000".parse().unwrap()],
    }
}
#[test]
fn records_round_trip_and_verify() {
    let node = iroh::SecretKey::generate();
    let mut dual = introduction(&node);
    if let RecordBody::Introduction { addrs, .. } = &mut dual {
        addrs.push("[::1]:7000".parse().unwrap());
    }
    for body in [
        introduction(&node),
        dual,
        RecordBody::Removal {
            fleet: "ab".repeat(32),
            signer: node.public().to_string(),
            counter: 2,
            subject: node.public().to_string(),
        },
    ] {
        let record = sign(body, &node);
        assert_eq!(record.verify(), Ok(()));
        assert_eq!(record.signer(), node.public().to_string());
        assert_eq!(record.fleet(), "ab".repeat(32));
        assert!(record.counter() > 0);
        assert_eq!(
            serde_json::from_slice::<SignedRecord>(&serde_json::to_vec(&record).unwrap()).unwrap(),
            record
        );
    }
}
#[test]
fn signatures_bind_body_key_and_domain() {
    let node = iroh::SecretKey::generate();
    let body = introduction(&node);
    let mut changed = sign(body.clone(), &node);
    if let RecordBody::Introduction { name, .. } = &mut changed.body {
        *name = "orchard".into();
    }
    let mut raw = sign(body.clone(), &node);
    raw.signature =
        data_encoding::HEXLOWER.encode(&node.sign(&serde_json::to_vec(&body).unwrap()).to_bytes());
    for record in [changed, raw, sign(body, &iroh::SecretKey::generate())] {
        assert_eq!(
            record.verify(),
            Err("the membership record's signature does not verify")
        );
    }
}
#[test]
fn invalid_fields_are_refused() {
    let node = iroh::SecretKey::generate();
    let base = serde_json::to_value(introduction(&node)).unwrap();
    let invalid_point = (0..=255)
        .map(|b| data_encoding::HEXLOWER.encode(&[b; 32]))
        .find(|k| crate::fleet::public_key(k).is_none())
        .unwrap();
    for (field, value) in [
        ("fleet", json!("AB".repeat(32))),
        ("fleet", json!("a".repeat(63))),
        ("inviter", json!(node.public().to_string().to_uppercase())),
        ("inviter", json!(invalid_point.clone())),
        ("inviter", json!("a".repeat(63))),
        ("newcomer", json!("a".repeat(63))),
        ("newcomer", json!("AB".repeat(32))),
        ("newcomer", json!(invalid_point.clone())),
        ("newcomer", json!(node.public().to_string())),
        ("counter", json!(0)),
        ("name", json!("Garden")),
        ("addrs", json!([])),
        (
            "addrs",
            json!(["127.0.0.1:7000", "[::1]:7000", "127.0.0.2:7000"]),
        ),
        ("addrs", json!(["0.0.0.0:7000"])),
        ("addrs", json!(["127.0.0.1:7000", "127.0.0.2:7000"])),
        ("addrs", json!(["[::ffff:127.0.0.1]:7000"])),
    ] {
        let mut body = base.clone();
        body[field] = value;
        assert_eq!(
            sign(serde_json::from_value(body).unwrap(), &node).verify(),
            Err("not a membership record"),
            "{field}"
        );
    }
    let removal = RecordBody::Removal {
        fleet: "ab".repeat(32),
        signer: node.public().to_string(),
        counter: 1,
        subject: node.public().to_string(),
    };
    for (field, value) in [
        ("fleet", json!("a")),
        ("signer", json!("a".repeat(63))),
        ("signer", json!("AB".repeat(32))),
        ("signer", json!(invalid_point.clone())),
        ("subject", json!(invalid_point)),
        ("subject", json!("a".repeat(63))),
        ("subject", json!("F".repeat(64))),
        ("counter", json!(0)),
    ] {
        let mut body = serde_json::to_value(&removal).unwrap();
        body[field] = value;
        assert_eq!(
            sign(serde_json::from_value(body).unwrap(), &node).verify(),
            Err("not a membership record")
        );
    }
    for signature in ["f".repeat(127), "F".repeat(128), "z".repeat(128)] {
        let mut record = sign(removal.clone(), &node);
        record.signature = signature;
        assert_eq!(record.verify(), Err("not a membership record"));
    }
}
#[test]
fn unknown_fields_and_kind_are_refused() {
    let node = iroh::SecretKey::generate();
    let base = serde_json::to_value(sign(introduction(&node), &node)).unwrap();
    let mut record = base.clone();
    record["extra"] = json!(true);
    assert!(serde_json::from_value::<SignedRecord>(record).is_err());
    let mut record = base.clone();
    record["body"]["extra"] = json!(true);
    assert!(serde_json::from_value::<SignedRecord>(record).is_err());
    let mut record = base;
    record["body"]["kind"] = json!("unknown");
    assert!(serde_json::from_value::<SignedRecord>(record).is_err());
}
