use serde::{Deserialize, Serialize};
use std::{fmt, net::SocketAddr};
pub const PREFIX: &str = "rj1";
pub const CODE_MAX_CHARS: usize = 2048;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Hints {
    pub direct: Vec<SocketAddr>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    pub fleet: String,
    pub inviter: iroh::EndpointId,
    pub secret: [u8; 32],
    pub hints: Hints,
}
impl Hints {
    fn valid(&self) -> bool {
        self.direct.len() <= 8
            && self
                .direct
                .iter()
                .all(|addr| crate::endpoint::usable_ip(&addr.ip()))
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 1024)
    }
}
impl JoinCode {
    pub fn new(
        fleet: String,
        inviter: iroh::EndpointId,
        hints: Hints,
    ) -> Result<Self, &'static str> {
        const MAKE: &str = "the join code could not be made";
        if !crate::wire::hex64(&fleet) || !hints.valid() {
            return Err(MAKE);
        }
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| MAKE)?;
        Ok(Self {
            fleet,
            inviter,
            secret,
            hints,
        })
    }
    pub fn encode(&self) -> String {
        use data_encoding::BASE64URL_NOPAD;
        format!(
            "{PREFIX}.{}.{}.{}.{}",
            self.fleet,
            self.inviter,
            BASE64URL_NOPAD.encode(&self.secret),
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&self.hints).expect("join hints serialize"))
        )
    }
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        use data_encoding::BASE64URL_NOPAD;
        const INVALID: &str = "not a join code";
        if text.len() > CODE_MAX_CHARS || !text.is_ascii() {
            return Err(INVALID);
        }
        let mut parts = text.split('.');
        if parts.next() != Some(PREFIX) {
            return Err(INVALID);
        }
        let fleet = parts.next().ok_or(INVALID)?;
        let inviter = parts
            .next()
            .and_then(crate::fleet::public_key)
            .ok_or(INVALID)?;
        let encoded_secret = parts.next().ok_or(INVALID)?;
        let encoded_hints = parts.next().ok_or(INVALID)?;
        if parts.next().is_some() || !crate::wire::hex64(fleet) {
            return Err(INVALID);
        }
        let secret: [u8; 32] = BASE64URL_NOPAD
            .decode(encoded_secret.as_bytes())
            .map_err(|_| INVALID)?
            .try_into()
            .map_err(|_| INVALID)?;
        if BASE64URL_NOPAD.encode(&secret) != encoded_secret {
            return Err(INVALID);
        }
        let bytes = BASE64URL_NOPAD
            .decode(encoded_hints.as_bytes())
            .map_err(|_| INVALID)?;
        if bytes.len() > 1024 {
            return Err(INVALID);
        }
        let hints: Hints = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        if !hints.valid() {
            return Err(INVALID);
        }
        Ok(Self {
            fleet: fleet.into(),
            inviter,
            secret,
            hints,
        })
    }
    pub fn hash(&self) -> [u8; 32] {
        use sha2::Digest;
        sha2::Sha256::digest(self.secret).into()
    }
}
impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinCode")
            .field("fleet", &self.fleet)
            .field("inviter", &self.inviter)
            .field("secret", &"<hidden>")
            .field("hints", &self.hints)
            .finish()
    }
}
#[cfg(test)]
#[path = "code_tests.rs"]
mod tests;
