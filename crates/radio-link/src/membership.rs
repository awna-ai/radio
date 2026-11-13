use crate::fleet::FleetRecord;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

pub const MEMBERSHIP_DOMAIN: &[u8] = b"radio membership 1\n";
pub const ADDRS_MAX: usize = 2;
pub const COMPUTERS_MAX: usize = 1024;
pub const REMOVED_MAX: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordBody {
    Introduction {
        fleet: String,
        inviter: String,
        counter: u64,
        newcomer: String,
        name: String,
        addrs: Vec<SocketAddr>,
    },
    Removal {
        fleet: String,
        signer: String,
        counter: u64,
        subject: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRecord {
    pub body: RecordBody,
    pub signature: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Known {
    pub key: String,
    pub name: String,
    pub invited_by: Option<String>,
    pub addrs: Vec<SocketAddr>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinAnswer {
    pub fleet: FleetRecord,
    pub computers: Vec<Known>,
    pub removed: Vec<String>,
}

fn signed_bytes(body: &RecordBody) -> Vec<u8> {
    let mut bytes = MEMBERSHIP_DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(body).expect("membership body serializes"));
    bytes
}
pub fn sign(body: RecordBody, key: &iroh::SecretKey) -> SignedRecord {
    SignedRecord {
        signature: data_encoding::HEXLOWER.encode(&key.sign(&signed_bytes(&body)).to_bytes()),
        body,
    }
}
impl SignedRecord {
    pub fn verify(&self) -> Result<(), &'static str> {
        const INVALID: &str = "not a membership record";
        let key = crate::fleet::public_key(self.signer()).ok_or(INVALID)?;
        let valid = match &self.body {
            RecordBody::Introduction {
                inviter,
                newcomer,
                name,
                addrs,
                ..
            } => {
                crate::fleet::public_key(newcomer).is_some()
                    && newcomer != inviter
                    && crate::wire::member_name(name)
                    && valid_addrs(addrs, false)
            }
            RecordBody::Removal { subject, .. } => crate::fleet::public_key(subject).is_some(),
        };
        if !valid
            || !crate::wire::hex64(self.fleet())
            || self.counter() == 0
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
            &signed_bytes(&self.body),
            &iroh::Signature::from_bytes(&bytes),
        )
        .map_err(|_| "the membership record's signature does not verify")
    }
    pub fn signer(&self) -> &str {
        match &self.body {
            RecordBody::Introduction { inviter, .. } => inviter,
            RecordBody::Removal { signer, .. } => signer,
        }
    }
    pub fn counter(&self) -> u64 {
        match self.body {
            RecordBody::Introduction { counter, .. } | RecordBody::Removal { counter, .. } => {
                counter
            }
        }
    }
    pub fn fleet(&self) -> &str {
        match &self.body {
            RecordBody::Introduction { fleet, .. } | RecordBody::Removal { fleet, .. } => fleet,
        }
    }
}
pub(crate) fn valid_addrs(addrs: &[SocketAddr], empty: bool) -> bool {
    (empty && addrs.is_empty())
        || (addrs.len() <= ADDRS_MAX
            && crate::endpoint::usable(addrs).is_ok_and(|canonical| canonical == addrs))
}

impl Known {
    pub(crate) fn valid(&self) -> bool {
        crate::fleet::public_key(&self.key).is_some()
            && crate::wire::member_name(&self.name)
            && self
                .invited_by
                .as_ref()
                .is_none_or(|key| crate::fleet::public_key(key).is_some())
            && valid_addrs(&self.addrs, true)
    }
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
