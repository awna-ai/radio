use super::*;
use crate::computer_record::{self, ComputerBody};
use crate::membership::{self, RecordBody};
use data_encoding::HEXLOWER;
use std::collections::BTreeMap;

#[test]
fn fleet_topic_ids_are_stable_distinct_and_domain_separated() {
    let fleet = "ab".repeat(32);
    assert_eq!(topic_id(&fleet), topic_id(&fleet));
    assert_ne!(topic_id(&fleet), topic_id(&"cd".repeat(32)));
    let bare: [u8; 32] = Sha256::digest(fleet.as_bytes()).into();
    assert_ne!(topic_id(&fleet), iroh_gossip::TopicId::from(bare));
    let mut hash = Sha256::new();
    hash.update(b"radio fleet topic 1\n");
    hash.update(fleet.as_bytes());
    let expected: [u8; 32] = hash.finalize().into();
    assert_eq!(topic_id(&fleet), iroh_gossip::TopicId::from(expected));
}

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

#[test]
fn announcer_first_change_is_due_once() {
    let node = iroh::SecretKey::generate();
    let record = record(&node, &"ab".repeat(32), 1);
    let mut announcer = Announcer::new();
    assert_eq!(announcer.due(0.0), None);
    announcer.changed(record.clone());
    assert_eq!(announcer.due(0.0), Some(record));
    assert_eq!(announcer.due(600.0), None);
    assert_eq!(announcer.next_due(600.0), None);
}

#[test]
fn announcer_coalesces_changes_until_exactly_ten_minutes() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let mut announcer = Announcer::new();
    announcer.changed(record(&node, &fleet, 1));
    assert!(announcer.due(0.0).is_some());
    announcer.changed(record(&node, &fleet, 2));
    assert_eq!(announcer.due(60.0), None);
    let newest = record(&node, &fleet, 3);
    announcer.changed(newest.clone());
    assert_eq!(announcer.due(120.0), None);
    assert_eq!(announcer.due(599.0), None);
    assert_eq!(announcer.due(600.0), Some(newest));
    assert_eq!(announcer.due(1200.0), None);
}

#[test]
fn announcer_keeps_the_newest_pending_record_including_equal_versions() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let newest = record(&node, &fleet, 3);
    let mut equal = newest.body.clone();
    equal.addrs.clear();
    let mut announcer = Announcer::new();
    announcer.changed(newest.clone());
    announcer.changed(computer_record::sign(equal, &node));
    announcer.changed(record(&node, &fleet, 2));
    assert_eq!(announcer.due(0.0), Some(newest));
}

#[test]
fn announcer_never_queues_a_record_at_or_below_the_one_announced() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let sent = record(&node, &fleet, 3);
    let mut equal = sent.body.clone();
    equal.addrs.clear();
    let mut announcer = Announcer::new();
    announcer.changed(sent.clone());
    assert_eq!(announcer.due(0.0), Some(sent));
    for stale in [
        record(&node, &fleet, 2),
        computer_record::sign(equal, &node),
    ] {
        announcer.changed(stale);
        assert_eq!(announcer.next_due(600.0), None);
        assert_eq!(announcer.due(600.0), None);
    }
    let newer = record(&node, &fleet, 4);
    announcer.changed(newer.clone());
    assert_eq!(announcer.next_due(600.0), Some(600.0));
    assert_eq!(announcer.due(600.0), Some(newer));
}

#[test]
fn announcer_ignores_a_clock_that_is_not_finite() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let first = record(&node, &fleet, 1);
    let mut announcer = Announcer::new();
    announcer.changed(first.clone());
    for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(announcer.due(now), None);
        assert_eq!(announcer.next_due(now), None);
    }
    assert_eq!(announcer.due(0.0), Some(first));
    let newer = record(&node, &fleet, 2);
    announcer.changed(newer.clone());
    for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(announcer.due(now), None);
        assert_eq!(announcer.next_due(now), None);
    }
    assert_eq!(announcer.next_due(1.0), Some(600.0));
    assert_eq!(announcer.due(600.0), Some(newer));
}

#[test]
fn announcer_next_due_tracks_pending_changes() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let mut announcer = Announcer::new();
    assert_eq!(announcer.next_due(10.0), None);
    announcer.changed(record(&node, &fleet, 1));
    assert_eq!(announcer.next_due(10.0), Some(10.0));
    assert!(announcer.due(10.0).is_some());
    assert_eq!(announcer.next_due(20.0), None);
    announcer.changed(record(&node, &fleet, 2));
    assert_eq!(announcer.next_due(20.0), Some(610.0));
    assert_eq!(announcer.next_due(700.0), Some(700.0));
}

#[test]
fn announcer_due_handles_a_clock_that_went_backwards() {
    let node = iroh::SecretKey::generate();
    let fleet = "ab".repeat(32);
    let mut announcer = Announcer::new();
    announcer.changed(record(&node, &fleet, 1));
    assert!(announcer.due(100.0).is_some());
    let newest = record(&node, &fleet, 2);
    announcer.changed(newest.clone());
    assert_eq!(announcer.next_due(99.0), Some(99.0));
    assert_eq!(announcer.due(99.0), Some(newest));
    announcer.changed(record(&node, &fleet, 3));
    assert_eq!(announcer.next_due(100.0), Some(699.0));

    for now in [99.0, 100.0, 699.0, 700.0] {
        let mut announcer = Announcer::new();
        announcer.changed(record(&node, &fleet, 1));
        assert!(announcer.due(100.0).is_some());
        announcer.changed(record(&node, &fleet, 2));
        assert_eq!(
            announcer.next_due(now) == Some(now),
            announcer.due(now).is_some(),
            "{now}"
        );
    }
}
