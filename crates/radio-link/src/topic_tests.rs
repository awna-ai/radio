use super::*;
use crate::computer_record::{self, ComputerBody};
use crate::membership::{self, RecordBody};
use data_encoding::HEXLOWER;
use std::collections::BTreeMap;

fn record(node: &iroh::SecretKey, fleet: &str, version: u64) -> ComputerRecord {
    computer_record::sign(
        ComputerBody {
            fleet: fleet.into(),
            key: node.public().to_string(),
            version,
            name: "garden".into(),
            roles: vec![],
            addrs: vec!["127.0.0.1:7000".parse().unwrap()],
            apps: BTreeMap::new(),
        },
        node,
    )
}

#[test]
fn an_announcement_verifies_only_for_its_fleet_and_its_signer() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let announcement = announce(&record(&node, &fleet, 5), &node);
    assert_eq!(announcement.verify(&fleet), Ok(()));
    assert_eq!(announcement.key, node.public().to_string());
    assert_eq!(announcement.version, 5);
    assert_eq!(
        announcement.verify(&"cd".repeat(32)),
        Err("the announcement's signature does not verify")
    );
    let mut forged = announce(&record(&node, &fleet, 5), &iroh::SecretKey::generate());
    forged.key = node.public().to_string();
    assert_eq!(
        forged.verify(&fleet),
        Err("the announcement's signature does not verify")
    );
    for change in [
        |a: &mut Announcement| a.version += 1,
        |a: &mut Announcement| a.hash = "0".repeat(64),
    ] {
        let mut changed = announcement.clone();
        change(&mut changed);
        assert_eq!(
            changed.verify(&fleet),
            Err("the announcement's signature does not verify")
        );
    }
}

#[test]
fn a_malformed_announcement_is_refused_before_its_signature() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let announcement = announce(&record(&node, &fleet, 5), &node);
    for change in [
        |a: &mut Announcement| a.version = 0,
        |a: &mut Announcement| a.version = i64::MAX as u64 + 1,
        |a: &mut Announcement| a.hash = "AB".repeat(32),
        |a: &mut Announcement| {
            a.hash.pop();
        },
        |a: &mut Announcement| a.key = "zz".repeat(32),
        |a: &mut Announcement| a.signature.push('0'),
        |a: &mut Announcement| a.signature = "zz".repeat(64),
    ] {
        let mut changed = announcement.clone();
        change(&mut changed);
        assert_eq!(changed.verify(&fleet), Err("not an announcement"));
    }
}

#[test]
fn an_announcement_matches_only_its_record() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let announced = record(&node, &fleet, 5);
    let announcement = announce(&announced, &node);
    assert_eq!(announcement.hash, record_hash(&announced));
    assert!(announcement.matches(&announced));
    let mut other_content = announced.body.clone();
    other_content.addrs = vec![];
    let other = iroh::SecretKey::generate();
    for record in [
        record(&node, &fleet, 4),
        record(&node, &fleet, 6),
        computer_record::sign(other_content, &node),
        record(&other, &fleet, 5),
    ] {
        assert!(!announcement.matches(&record), "{record:?}");
    }
}

fn signed_record() -> SignedRecord {
    let node = iroh::SecretKey::generate();
    membership::sign(
        RecordBody::Removal {
            fleet: "ab".repeat(32),
            signer: node.public().to_string(),
            counter: 1,
            subject: iroh::SecretKey::generate().public().to_string(),
        },
        &node,
    )
}

#[test]
fn an_announcement_is_137_bytes_in_a_fixed_layout() {
    let node = iroh::SecretKey::generate();
    let announcement = announce(&record(&node, &"ab".repeat(32), 5), &node);
    let message = TopicMessage::Announcement(announcement.clone());
    let bytes = message.encode().unwrap();
    assert_eq!(bytes.len(), 137);
    assert_eq!(bytes[0], TAG_ANNOUNCEMENT);
    assert_eq!(&bytes[1..33], node.public().as_bytes());
    assert_eq!(bytes[33..41], 5u64.to_be_bytes());
    assert_eq!(
        bytes[41..73],
        HEXLOWER.decode(announcement.hash.as_bytes()).unwrap()
    );
    assert_eq!(
        bytes[73..],
        HEXLOWER.decode(announcement.signature.as_bytes()).unwrap()
    );
    assert_eq!(TopicMessage::decode(&bytes), Ok(message));
}

#[test]
fn the_signed_bytes_are_fixed() {
    assert_eq!(
        signed_bytes("f", "k", 7, "h"),
        b"radio computer announcement 1\n{\"fleet\":\"f\",\"key\":\"k\",\"version\":7,\"hash\":\"h\"}"
    );
}

#[test]
fn a_record_is_its_tag_then_its_signed_json() {
    let record = signed_record();
    let mut expected = vec![1];
    expected.extend(serde_json::to_vec(&record).unwrap());
    let message = TopicMessage::Record(record);
    let bytes = message.encode().unwrap();
    assert_eq!(bytes[0], TAG_RECORD);
    assert_eq!(bytes, expected);
    assert_eq!(TopicMessage::decode(&bytes), Ok(message));
}

#[test]
fn an_announcement_of_any_other_length_is_dropped_unread() {
    let node = iroh::SecretKey::generate();
    let mut bytes =
        TopicMessage::Announcement(announce(&record(&node, &"ab".repeat(32), 5), &node))
            .encode()
            .unwrap();
    assert_eq!(
        TopicMessage::decode(&bytes[..136]),
        Err("not a topic message")
    );
    bytes.push(0);
    assert_eq!(TopicMessage::decode(&bytes), Err("not a topic message"));
}

#[test]
fn an_unknown_tag_or_an_empty_message_is_refused() {
    let node = iroh::SecretKey::generate();
    let announcement = announce(&record(&node, &"ab".repeat(32), 5), &node);
    let mut unknown = TopicMessage::Announcement(announcement.clone())
        .encode()
        .unwrap();
    unknown[0] = 0xff;
    let mut null = vec![TAG_RECORD];
    null.extend(b"null");
    let mut wrapped_record = vec![TAG_RECORD];
    wrapped_record
        .extend(serde_json::to_vec(&serde_json::json!({"record": signed_record()})).unwrap());
    let old_announcement =
        serde_json::to_vec(&serde_json::json!({"announcement": announcement})).unwrap();
    let record = serde_json::to_vec(&signed_record()).unwrap();
    let unknown_record = |tag: u8| [vec![tag], record.clone()].concat();
    for bytes in [
        vec![],
        vec![0],
        vec![3],
        unknown,
        null,
        wrapped_record,
        old_announcement,
        unknown_record(0),
        unknown_record(3),
    ] {
        assert_eq!(TopicMessage::decode(&bytes), Err("not a topic message"));
    }
}

#[test]
fn any_137_announcement_bytes_decode_to_one_form() {
    let mut bytes = vec![TAG_ANNOUNCEMENT];
    bytes.extend((0..136).map(|i| (i as u8).wrapping_mul(7)));
    let message = TopicMessage::decode(&bytes).unwrap();
    assert_eq!(message.encode().unwrap(), bytes);
    let TopicMessage::Announcement(announcement) = message else {
        panic!("the announcement tag decodes to an announcement");
    };
    assert!(announcement.verify(&"ab".repeat(32)).is_err());
}

#[test]
fn the_cap_is_checked_before_decoding_a_record() {
    let message = TopicMessage::Record(signed_record());
    let mut bytes = message.encode().unwrap();
    bytes.resize(TOPIC_MESSAGE_MAX, b' ');
    assert_eq!(TopicMessage::decode(&bytes), Ok(message));
    bytes.push(b' ');
    assert_eq!(
        TopicMessage::decode(&bytes),
        Err("the topic message is too large")
    );
}

#[test]
fn an_announcement_with_malformed_fields_does_not_encode() {
    let node = iroh::SecretKey::generate();
    let announcement = announce(&record(&node, &"ab".repeat(32), 5), &node);
    for change in [
        |a: &mut Announcement| a.hash = "AB".repeat(32),
        |a: &mut Announcement| a.key = "a".repeat(63),
        |a: &mut Announcement| a.signature = "a".repeat(127),
    ] {
        let mut changed = announcement.clone();
        change(&mut changed);
        assert_eq!(
            TopicMessage::Announcement(changed).encode(),
            Err("not an announcement")
        );
    }
}

#[test]
fn the_largest_announcement_is_far_under_the_cap() {
    let node = iroh::SecretKey::generate();
    let bytes = TopicMessage::Announcement(announce(
        &record(&node, &"ab".repeat(32), i64::MAX as u64),
        &node,
    ))
    .encode()
    .unwrap();
    eprintln!("announcement={}", bytes.len());
    assert_eq!(bytes.len(), ANNOUNCEMENT_LEN);
}
