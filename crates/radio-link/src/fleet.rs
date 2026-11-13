use serde::{Deserialize, Serialize};
pub const RECORD_DOMAIN: &[u8] = b"radio fleet record 1\n";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetBody {
    pub id: String,
    pub name: String,
    pub version: u64,
    pub founder: String,
    pub recovery: String,
    pub release_key: Option<String>,
    pub relays: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Signer {
    Founder,
    Recovery,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetRecord {
    pub body: FleetBody,
    pub signer: Signer,
    pub signature: String,
}
fn signed_bytes(body: &FleetBody) -> Vec<u8> {
    let mut bytes = RECORD_DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(body).expect("fleet body serializes"));
    bytes
}

pub fn sign(body: FleetBody, signer: Signer, key: &iroh::SecretKey) -> FleetRecord {
    let signature = data_encoding::HEXLOWER.encode(&key.sign(&signed_bytes(&body)).to_bytes());
    FleetRecord {
        body,
        signer,
        signature,
    }
}

pub(crate) fn public_key(hex: &str) -> Option<iroh::PublicKey> {
    if !crate::wire::hex64(hex) {
        return None;
    }
    let bytes: [u8; 32] = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .ok()?
        .try_into()
        .ok()?;
    iroh::PublicKey::from_bytes(&bytes).ok()
}

impl FleetRecord {
    pub fn verify(&self) -> Result<(), &'static str> {
        const INVALID: &str = "not a fleet record";
        let body = &self.body;
        let founder = public_key(&body.founder).ok_or(INVALID)?;
        let recovery = public_key(&body.recovery).ok_or(INVALID)?;
        if !crate::wire::hex64(&body.id)
            || !crate::wire::member_name(&body.name)
            || body.version == 0
            || founder == recovery
            || !body.relays.is_empty()
            || body
                .release_key
                .as_ref()
                .is_some_and(|key| public_key(key).is_none())
            || self.signature.len() != 128
        {
            return Err(INVALID);
        }
        let bytes: [u8; 64] = data_encoding::HEXLOWER
            .decode(self.signature.as_bytes())
            .map_err(|_| INVALID)?
            .try_into()
            .map_err(|_| INVALID)?;
        let key = match self.signer {
            Signer::Founder => founder,
            Signer::Recovery => recovery,
        };
        key.verify(&signed_bytes(body), &iroh::Signature::from_bytes(&bytes))
            .map_err(|_| "the fleet record's signature does not verify")
    }
}

pub fn accept(current: Option<&FleetRecord>, new: &FleetRecord) -> Result<(), &'static str> {
    new.verify()?;
    let Some(current) = current else {
        return Ok(());
    };
    if new.body.id != current.body.id {
        return Err("the fleet record is for another fleet");
    }
    if new.body.version <= current.body.version {
        return Err("the fleet record is not newer");
    }
    if new.body.recovery != current.body.recovery {
        return Err("the fleet record changes its recovery key");
    }
    match new.signer {
        Signer::Founder if new.body.founder != current.body.founder => {
            Err("only the recovery key names a new founding member")
        }
        Signer::Recovery
            if new.body.name != current.body.name || new.body.relays != current.body.relays =>
        {
            Err("the recovery key changes only the founding member and the release key")
        }
        _ => Ok(()),
    }
}

pub fn found(
    node: &iroh::SecretKey,
    name: &str,
) -> Result<(FleetRecord, iroh::SecretKey), &'static str> {
    if !crate::wire::member_name(name) {
        return Err("not a fleet name");
    }
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|_| "the fleet could not be made")?;
    let recovery = loop {
        let key = iroh::SecretKey::generate();
        if key.public() != node.public() {
            break key;
        }
    };
    let body = FleetBody {
        id: data_encoding::HEXLOWER.encode(&id),
        name: name.into(),
        version: 1,
        founder: node.public().to_string(),
        recovery: recovery.public().to_string(),
        release_key: None,
        relays: vec![],
    };
    Ok((sign(body, Signer::Founder, node), recovery))
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
