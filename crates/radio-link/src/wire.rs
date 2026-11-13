use serde::{Deserialize, Serialize};

pub const LINK_MAJOR: u32 = 1;
pub const LINK_MINOR: u32 = 3;
pub const NO_FOLDER: &str = "no_folder";
pub const EXISTS: &str = "exists";
pub const BUSY: &str = "busy";
pub const FOLDER: &str = "folder";
pub const WRITE: &str = "write";
pub const FULL: &str = "full";
pub const SPACE: &str = "space";
pub const FOLDER_FILES: usize = 64;
pub const FOLDER_BYTES: u64 = 1 << 30;
pub const FILE_CAP: u64 = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    pub code: String,
    pub consent: String,
    pub name: String,
    pub addrs: Vec<std::net::SocketAddr>,
}

impl std::fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinRequest")
            .field("code", &"<hidden>")
            .field("consent", &"<hidden>")
            .field("name", &self.name)
            .field("addrs", &self.addrs)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    File {
        to: String,
        app: String,
        name: String,
        size: u64,
    },
    Computer {
        record: crate::computer_record::ComputerRecord,
    },
    Record {
        record: crate::membership::SignedRecord,
    },
    Hello {
        hello: Hello,
    },
    App {
        to: String,
        app: String,
        body: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields,
    from = "AnswerFields"
)]
pub enum Answer {
    Refused { reason: String },
    Hello { hello: Hello },
    Ok,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum AnswerFields {
    Refused { reason: String },
    Hello { hello: Hello },
    Ok {},
}

impl From<AnswerFields> for Answer {
    fn from(fields: AnswerFields) -> Self {
        match fields {
            AnswerFields::Refused { reason } => Self::Refused { reason },
            AnswerFields::Hello { hello } => Self::Hello { hello },
            AnswerFields::Ok {} => Self::Ok,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol: Protocol,
    pub fleet: String,
    pub sha: String,
    pub arch: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protocol {
    pub major: u32,
    pub minor: u32,
}

pub fn app_name(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 64
        && (app.as_bytes()[0].is_ascii_lowercase() || app.as_bytes()[0].is_ascii_digit())
        && app.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

pub fn member_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn valid_hello(hello: &Hello) -> bool {
    hello.protocol.major == LINK_MAJOR
        && hex64(&hello.fleet)
        && hex64(&hello.sha)
        && !hello.arch.is_empty()
        && hello.arch.len() <= 16
        && hello
            .arch
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && member_name(&hello.name)
}

pub(crate) fn valid_app(to: &str, app: &str, local: &iroh::EndpointId) -> bool {
    hex64(to) && to == local.to_string() && app_name(app)
}

pub fn file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub(crate) fn valid_file(
    to: &str,
    app: &str,
    name: &str,
    size: u64,
    local: &iroh::EndpointId,
) -> bool {
    valid_app(to, app, local) && file_name(name) && size <= FILE_CAP
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
