//! The fleet's gossip topic: the messages it carries, and the check each one passes before it is
//! applied. Introductions and removals travel as the signed membership records they are; a
//! computer's own record never travels on the topic, only a signed announcement of its version and
//! hash, and a member that needs the record fetches it and checks it against the announcement.

use crate::{
    computer_record::ComputerRecord,
    fleet_db::{Applied, FleetDb},
    membership::SignedRecord,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ANNOUNCEMENT_DOMAIN: &[u8] = b"radio computer announcement 1\n";
/// iroh-gossip's default `max_message_size`; a larger message is dropped unread.
pub const TOPIC_MESSAGE_MAX: usize = 4096;
const INVALID: &str = "not a topic message";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TopicMessage {
    Record(SignedRecord),
    Announcement(Announcement),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Record(Applied),
    Announcement,
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

impl TopicMessage {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a topic message serializes")
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > TOPIC_MESSAGE_MAX {
            return Err("the topic message is too large");
        }
        serde_json::from_slice(bytes).map_err(|_| INVALID)
    }
}

/// Checks one message from the fleet's topic and applies it: a membership record through
/// `FleetDb::apply`, an announcement through `FleetDb::apply_announcement`. Anything that fails a
/// check is dropped with nothing written.
pub fn receive(
    db: &mut FleetDb,
    node: &iroh::EndpointId,
    own_name: &str,
    bytes: &[u8],
    now: f64,
) -> Result<Received, &'static str> {
    match TopicMessage::decode(bytes)? {
        TopicMessage::Record(record) => {
            db.apply(node, own_name, &record, now).map(Received::Record)
        }
        TopicMessage::Announcement(announcement) => db
            .apply_announcement(node, &announcement)
            .map(|()| Received::Announcement),
    }
}

#[cfg(test)]
#[path = "topic_tests.rs"]
mod tests;
