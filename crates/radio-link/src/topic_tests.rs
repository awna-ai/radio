use super::*;
use crate::computer_record::{self, ComputerBody};
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

#[test]
fn topic_messages_round_trip_and_refuse_extra_fields_and_large_messages() {
    let node = iroh::SecretKey::generate();
    let message = TopicMessage::Announcement(announce(&record(&node, &"ab".repeat(32), 5), &node));
    let bytes = message.encode();
    assert_eq!(TopicMessage::decode(&bytes), Ok(message));
    let mut extra: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    extra["announcement"]["fleet"] = "ab".repeat(32).into();
    assert_eq!(
        TopicMessage::decode(&serde_json::to_vec(&extra).unwrap()),
        Err("not a topic message")
    );
    assert_eq!(
        TopicMessage::decode(br#"{"record":null}"#),
        Err("not a topic message")
    );
    let mut padded = bytes.clone();
    padded.resize(TOPIC_MESSAGE_MAX, b' ');
    assert!(TopicMessage::decode(&padded).is_ok());
    padded.push(b' ');
    assert_eq!(
        TopicMessage::decode(&padded),
        Err("the topic message is too large")
    );
}

#[test]
fn the_largest_announcement_is_far_under_the_cap() {
    let node = iroh::SecretKey::generate();
    let bytes = TopicMessage::Announcement(announce(
        &record(&node, &"ab".repeat(32), i64::MAX as u64),
        &node,
    ))
    .encode();
    eprintln!("announcement={}", bytes.len());
    assert!(bytes.len() <= 512, "{}", bytes.len());
}
