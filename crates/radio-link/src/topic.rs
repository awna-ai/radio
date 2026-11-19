//! The fleet's gossip topic: the messages it carries, and the check each one passes before it is
//! applied. Introductions and removals travel as the signed membership records they are; a
//! computer's own record never travels on the topic, only a signed announcement of its version and
//! hash, and a member that needs the record fetches it and checks it against the announcement.
//!
//! Every message starts with one tag byte. Tag 1 carries a signed membership record's JSON bytes,
//! without a wrapper. Tag 2 carries an announcement in exactly 137 bytes, including the tag:
//! bytes 1..33 are the public key, 33..41 the version as a big-endian u64, 41..73 the SHA-256 hash,
//! and 73..137 the signature. The announcement's signed bytes stay unchanged.

use crate::{
    computer_record::ComputerRecord,
    fleet_db::{Applied, FleetDb},
    membership::SignedRecord,
};
use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const ANNOUNCEMENT_DOMAIN: &[u8] = b"radio computer announcement 1\n";
/// iroh-gossip's default `max_message_size`; a larger message is dropped unread. It bounds the
/// whole gossip frame, so what a member can send is a few dozen bytes less.
pub const TOPIC_MESSAGE_MAX: usize = 4096;
pub const TAG_RECORD: u8 = 1;
pub const TAG_ANNOUNCEMENT: u8 = 2;
pub const ANNOUNCEMENT_LEN: usize = 137;
const INVALID: &str = "not a topic message";

pub const ANNOUNCEMENT_RECEIVE_WINDOW: f64 = 300.0;
// Ten minutes on the sender against five on the receiver leaves room for clock skew.
pub const ANNOUNCEMENT_SEND_WINDOW: f64 = 600.0;

/// Held in memory beside `FleetDb`, never in `fleet.db`, so a restart clears it.
/// Bounds how often an announcement from one computer is kept and acted on, not the topic's
/// traffic (iroh-gossip forwards a message before the application sees it) and not the cost of
/// checking a forgery.
/// An announcement dropped inside the window is not kept or retried here. The version
/// it named arrives with the computer's next announcement after the window, or with
/// the catch-up a later step adds.
#[derive(Debug, Default)]
pub struct ReceiveBar {
    kept: HashMap<String, f64>,
}

impl ReceiveBar {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Coalesces this computer's changes; the caller signs and sends the returned record.
#[derive(Debug, Default)]
pub struct Announcer {
    last: Option<f64>,
    sent: Option<u64>,
    pending: Option<ComputerRecord>,
}

impl Announcer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn changed(&mut self, record: ComputerRecord) {
        if self.sent.is_some_and(|v| record.body.version <= v) {
            return;
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.body.version < record.body.version)
        {
            self.pending = Some(record);
        }
    }

    /// The returned record counts as announced; if the caller's send fails, that version is not
    /// offered again and goes out with the next change.
    pub fn due(&mut self, now: f64) -> Option<ComputerRecord> {
        if !now.is_finite() {
            return None;
        }
        let due = self
            .last
            .is_none_or(|last| now < last || now - last >= ANNOUNCEMENT_SEND_WINDOW);
        if !due {
            return None;
        }
        let record = self.pending.take()?;
        self.last = Some(now);
        self.sent = Some(record.body.version);
        Some(record)
    }

    pub fn next_due(&self, now: f64) -> Option<f64> {
        if !now.is_finite() {
            return None;
        }
        self.pending.as_ref()?;
        Some(self.last.map_or(now, |last| {
            if now < last {
                now
            } else {
                now.max(last + ANNOUNCEMENT_SEND_WINDOW)
            }
        }))
    }
}

/// A computer's signed statement that its record is now at `version`, with SHA-256 `hash` of the
/// record's bytes. The signature also covers the fleet id, which is not sent: the topic is per fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announcement {
    pub key: String,
    pub version: u64,
    pub hash: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicMessage {
    Record(SignedRecord),
    Announcement(Announcement),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Record(Applied),
    Announcement(Announcement),
}

#[derive(Serialize)]
struct AnnouncementBody<'a> {
    fleet: &'a str,
    key: &'a str,
    version: u64,
    hash: &'a str,
}
fn signed_bytes(fleet: &str, key: &str, version: u64, hash: &str) -> Vec<u8> {
    let mut bytes = ANNOUNCEMENT_DOMAIN.to_vec();
    bytes.extend(
        serde_json::to_vec(&AnnouncementBody {
            fleet,
            key,
            version,
            hash,
        })
        .expect("announcement body serializes"),
    );
    bytes
}

/// The SHA-256 of a computer record's bytes, as `serde_json::to_vec` writes them.
pub fn record_hash(record: &ComputerRecord) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(
        serde_json::to_vec(record).expect("computer record serializes"),
    ))
}

/// Announces `record`, signed with its computer's `key`.
pub fn announce(record: &ComputerRecord, key: &iroh::SecretKey) -> Announcement {
    let body = &record.body;
    let hash = record_hash(record);
    let signature = key.sign(&signed_bytes(&body.fleet, &body.key, body.version, &hash));
    Announcement {
        key: body.key.clone(),
        version: body.version,
        hash,
        signature: data_encoding::HEXLOWER.encode(&signature.to_bytes()),
    }
}

impl Announcement {
    pub fn verify(&self, fleet: &str) -> Result<(), &'static str> {
        const INVALID: &str = "not an announcement";
        let key = crate::fleet::public_key(&self.key).ok_or(INVALID)?;
        if self.version == 0
            || self.version > i64::MAX as u64
            || !crate::wire::hex64(&self.hash)
            || self.signature.len() != 128
        {
            return Err(INVALID);
        }
        let bytes: [u8; 64] = data_encoding::HEXLOWER
            .decode(self.signature.as_bytes())
            .map_err(|_| INVALID)?
            .try_into()
            .map_err(|_| INVALID)?;
        key.verify(
            &signed_bytes(fleet, &self.key, self.version, &self.hash),
            &iroh::Signature::from_bytes(&bytes),
        )
        .map_err(|_| "the announcement's signature does not verify")
    }

    /// Whether `record` is the one this announcement names: same key, version and hash.
    pub fn matches(&self, record: &ComputerRecord) -> bool {
        record.body.key == self.key
            && record.body.version == self.version
            && record_hash(record) == self.hash
    }
}

/// `hex` as exactly `N` bytes, when it is lowercase hex of that length.
fn hex_bytes<const N: usize>(hex: &str) -> Option<[u8; N]> {
    HEXLOWER.decode(hex.as_bytes()).ok()?.try_into().ok()
}

impl TopicMessage {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        match self {
            Self::Record(record) => {
                let mut bytes = vec![TAG_RECORD];
                bytes.extend(serde_json::to_vec(record).expect("a topic message serializes"));
                Ok(bytes)
            }
            Self::Announcement(announcement) => {
                const INVALID: &str = "not an announcement";
                let key: [u8; 32] = hex_bytes(&announcement.key).ok_or(INVALID)?;
                let hash: [u8; 32] = hex_bytes(&announcement.hash).ok_or(INVALID)?;
                let signature: [u8; 64] = hex_bytes(&announcement.signature).ok_or(INVALID)?;
                let mut bytes = Vec::with_capacity(ANNOUNCEMENT_LEN);
                bytes.push(TAG_ANNOUNCEMENT);
                bytes.extend(key);
                bytes.extend(announcement.version.to_be_bytes());
                bytes.extend(hash);
                bytes.extend(signature);
                Ok(bytes)
            }
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > TOPIC_MESSAGE_MAX {
            return Err("the topic message is too large");
        }
        let (&tag, rest) = bytes.split_first().ok_or(INVALID)?;
        match tag {
            TAG_RECORD => serde_json::from_slice::<SignedRecord>(rest)
                .map(Self::Record)
                .map_err(|_| INVALID),
            TAG_ANNOUNCEMENT => {
                if bytes.len() != ANNOUNCEMENT_LEN {
                    return Err(INVALID);
                }
                let mut version = [0; 8];
                version.copy_from_slice(&bytes[33..41]);
                Ok(Self::Announcement(Announcement {
                    key: HEXLOWER.encode(&bytes[1..33]),
                    version: u64::from_be_bytes(version),
                    hash: HEXLOWER.encode(&bytes[41..73]),
                    signature: HEXLOWER.encode(&bytes[73..137]),
                }))
            }
            _ => Err(INVALID),
        }
    }
}

/// Checks one message from the fleet's topic and applies it: a membership record through
/// `FleetDb::apply`, an announcement through `FleetDb::apply_announcement`. Anything that fails a
/// check is dropped with nothing written.
pub fn receive(
    db: &mut FleetDb,
    bar: &mut ReceiveBar,
    node: &iroh::EndpointId,
    own_name: &str,
    bytes: &[u8],
    now: f64,
) -> Result<Received, &'static str> {
    match TopicMessage::decode(bytes)? {
        TopicMessage::Record(record) => {
            db.apply(node, own_name, &record, now).map(Received::Record)
        }
        TopicMessage::Announcement(announcement) => {
            if !now.is_finite() {
                return Err("the clock is not a finite number");
            }
            if bar
                .kept
                .get(&announcement.key)
                .is_some_and(|last| (0.0..ANNOUNCEMENT_RECEIVE_WINDOW).contains(&(now - last)))
            {
                return Err("an announcement from this computer was kept less than 5 minutes ago");
            }
            db.apply_announcement(node, &announcement)?;
            bar.kept.insert(announcement.key.clone(), now);
            Ok(Received::Announcement(announcement))
        }
    }
}

#[cfg(test)]
#[path = "topic_tests.rs"]
mod tests;
