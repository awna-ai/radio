use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;

pub const COMPUTER_DOMAIN: &[u8] = b"radio computer record 1\n";
pub const ROLES_MAX: usize = 8;
pub const APPS_MAX: usize = 8;
pub const SECTION_MAX: usize = 4096;
pub const COMPUTER_RECORD_MAX: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerBody {
    pub fleet: String,
    pub key: String,
    pub version: u64,
    pub name: String,
    pub roles: Vec<String>,
    pub addrs: Vec<SocketAddr>,
    pub apps: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerRecord {
    pub body: ComputerBody,
    pub signature: String,
}

fn signed_bytes(body: &ComputerBody) -> Vec<u8> {
    let mut bytes = COMPUTER_DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(body).expect("computer body serializes"));
    bytes
}

pub fn sign(body: ComputerBody, key: &iroh::SecretKey) -> ComputerRecord {
    ComputerRecord {
        signature: data_encoding::HEXLOWER.encode(&key.sign(&signed_bytes(&body)).to_bytes()),
        body,
    }
}

pub fn valid_role(role: &str) -> bool {
    crate::wire::member_name(role)
}

impl ComputerRecord {
    pub fn verify(&self) -> Result<(), &'static str> {
        const INVALID: &str = "not a computer record";
        let body = &self.body;
        let key = crate::fleet::public_key(&body.key).ok_or(INVALID)?;
        if !crate::wire::hex64(&body.fleet)
            || body.version == 0
            || body.version > i64::MAX as u64
            || !crate::wire::member_name(&body.name)
            || body.roles.len() > ROLES_MAX
            || !body.roles.iter().all(|role| valid_role(role))
            || !body.roles.windows(2).all(|roles| roles[0] < roles[1])
            || !crate::membership::valid_addrs(&body.addrs, true)
            || body.apps.len() > APPS_MAX
            || !body.apps.iter().all(|(app, section)| {
                crate::wire::app_name(app) && !section.is_empty() && section.len() <= SECTION_MAX
            })
            || self.signature.len() != 128
        {
            return Err(INVALID);
        }
        let bytes: [u8; 64] = data_encoding::HEXLOWER
            .decode(self.signature.as_bytes())
            .map_err(|_| INVALID)?
            .try_into()
            .map_err(|_| INVALID)?;
        if serde_json::to_vec(self)
            .expect("computer record serializes")
            .len()
            > COMPUTER_RECORD_MAX
        {
            return Err("the computer record is too large");
        }
        key.verify(&signed_bytes(body), &iroh::Signature::from_bytes(&bytes))
            .map_err(|_| "the computer record's signature does not verify")
    }
}

#[cfg(test)]
#[path = "computer_record_tests.rs"]
mod tests;
