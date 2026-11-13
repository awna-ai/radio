//! The client side of Radio's socket contract, CONTRACT.md at the repo's root.
//! The contract's numbers live here as constants; a test checks them against CONTRACT.md.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod transport;
pub use transport::{NO_ANSWER, NOT_RUNNING, request, request_line};

/// The protocol major every request carries as `"v"`.
pub const PROTOCOL_MAJOR: u32 = 1;
/// The protocol minor of major 1 this crate speaks.
pub const PROTOCOL_MINOR: u32 = 7;

/// The ops major 1, minor 7 serves, in this order.
pub const OPS: &[&str] = &[
    "hello",
    "register",
    "create_room",
    "post",
    "status_set",
    "status_clear",
    "list_rooms",
    "read",
    "status",
    "id",
    "fleet_init",
    "code_create",
    "code_list",
    "code_revoke",
    "computer_list",
    "computer_accept",
    "computer_block",
    "computer_trust",
    "resign",
    "computer_records",
    "link_address_add",
    "link_address_list",
    "link_address_remove",
    "join",
    "computer_remove",
    "leave",
    "send",
    "delivery_folder_set",
    "delivery_folder_clear",
    "deliveries",
];
/// The maximum request line size, in bytes including its newline.
pub const REQUEST_MAX_BYTES: usize = 16 * 1024;
/// The maximum answer line size is 8 MiB in bytes; 500 JSON-escaped messages of 4 KiB fit.
pub const RESPONSE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// An operation sent to the daemon; unknown request fields are refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Asks for the daemon version, supported majors and available operations.
    Hello {},
    /// Replaces an app's whole registered member list.
    Register {
        app: String,
        /// The registering app's software version.
        app_version: String,
        members: Vec<Member>,
    },
    /// Creates a room homed on the serving computer.
    CreateRoom { room: String, title: String },
    /// Posts a message as a member.
    Post {
        room: String,
        author: Member,
        body: String,
    },
    /// Sets a status line and logs the change as a message.
    StatusSet {
        room: String,
        key: String,
        line: String,
        author: Member,
        /// When the status expires, in seconds since the Unix epoch, if set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<f64>,
    },
    /// Clears a status line and logs its removal when present.
    StatusClear {
        room: String,
        key: String,
        author: Member,
    },
    /// Lists a page of rooms in identifier order.
    ListRooms {
        /// Returns rooms whose identifiers are strictly greater than this string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        /// The room count, from 1 to 500; defaults to 500.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Reads the newest messages, or those after `since`, in ascending sequence order.
    Read {
        room: String,
        /// Returns only messages with a sequence number strictly greater than this value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        /// The message count, from 1 to 500; defaults to 50 for newest messages or 500 with `since`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Lists live status lines in key order.
    Status { room: String },
    /// Returns this computer's public link id, creating its key on the first call.
    Id {},
    /// Makes a fleet with this computer as its founding member.
    FleetInit { name: String },
    /// Makes a one-time join code for this computer's fleet.
    CodeCreate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<u64>,
    },
    /// Lists open join codes without their secrets.
    CodeList {},
    /// Closes an open join code by its identifier.
    CodeRevoke { id: String },
    /// Lists this computer's view of its fleet's other computers.
    ComputerList {},
    /// Lets a pending or blocked computer connect.
    ComputerAccept { computer: String },
    /// Refuses a computer on this computer only.
    ComputerBlock { computer: String },
    /// Trusts an accepted computer's future invitations.
    ComputerTrust { computer: String },
    /// Re-signs this computer's record with an app's section and optional roles.
    Resign {
        app: String,
        /// The app's new section; an empty string removes it, and leaving it out keeps it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<String>,
        /// The computer's new roles; leaving it out keeps them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roles: Option<Vec<String>>,
    },
    /// Lists this computer's record and its accepted computers' records with this app's section from each.
    ComputerRecords { app: String },
    /// Adds one of this computer's link addresses: an IP address, at most one per family.
    LinkAddressAdd { address: String },
    /// Lists this computer's link addresses and the fixed port.
    LinkAddressList {},
    /// Removes one of this computer's link addresses.
    LinkAddressRemove { address: String },
    /// Joins through a one-time code.
    Join { code: String },
    /// Removes a computer from the fleet.
    ComputerRemove { computer: String },
    /// Leaves this computer's fleet.
    Leave {},
    /// Sends a regular file to this app on one computer of the fleet.
    /// If the answer thread cannot start, the daemon answers `the send could not be started`.
    Send {
        app: String,
        computer: String,
        /// The file name to use in the receiving app's folder.
        name: String,
        /// The absolute source path, with a final link refused by the daemon.
        file: String,
    },
    /// Sets the private folder this app takes files in on this computer.
    DeliveryFolderSet { app: String, folder: String },
    /// Clears this app's delivery folder, if one was set.
    DeliveryFolderClear { app: String },
    /// Lists this app's arrivals in ascending id order.
    Deliveries {
        app: String,
        /// Returns only arrivals whose ids are strictly greater; defaults to zero.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        /// The arrival count, from 1 to 500; defaults to 100.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
}

/// A member identified by a kind and a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The member kind, with `human` reserved for the person at the computer.
    pub kind: String,
    /// The member name within its kind.
    pub name: String,
}

/// The protocol major and minor announced by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Protocol {
    /// The protocol major required on each request.
    pub major: u32,
    /// The additive protocol revision within this major.
    pub minor: u32,
}

/// A daemon answer that deliberately ignores unknown fields so newer minors remain readable.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Whether the operation succeeded.
    pub ok: bool,
    /// A fixed error phrase when the operation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True for another user, an unregistered member, or a status ownership refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<bool>,
    /// The supported majors when the requested version is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported: Option<Vec<u32>>,
    /// The daemon software version returned by hello.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio: Option<String>,
    /// The current protocol version returned by hello.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    /// The protocol majors served by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub majors: Option<Vec<u32>>,
    /// The operation names available on the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<Vec<String>>,
    /// The message sequence number within its room, including logged status changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Whether a status line, join code or link address was there and is now gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
    /// The rooms in identifier order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rooms: Option<Vec<Room>>,
    /// The requested messages in ascending sequence order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    /// The live status lines in key order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<StatusLine>>,
    /// This computer's public link id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The fleet made on this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<Fleet>,
    /// The file holding the recovery key to move off this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_file: Option<String>,
    /// The join code, including its secret, returned only when made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The identifier used to revoke the join code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_id: Option<String>,
    /// When the join code expires, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
    /// The open join codes, oldest first, without their secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codes: Option<Vec<JoinCodeInfo>>,
    /// This computer's view of its fleet's other computers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computers: Option<Vec<Computer>>,
    /// The computer accepted, blocked or trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<Computer>,
    /// This computer's re-signed record with the requesting app's section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<ComputerRecord>,
    /// This computer's record and its accepted computers' records by id, with the requesting app's sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<Vec<ComputerRecord>>,
    /// The fixed UDP port the link uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// This computer's link addresses, IPv4 first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addresses: Option<Vec<String>>,
    /// The number of computers a removal or leave reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<u64>,
    /// The bytes sent by a successful file send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// This app's arrivals in ascending id order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliveries: Option<Vec<Delivery>>,
}

/// A file that arrived for the requesting app on this computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// The arrival id used as the next page's cursor.
    pub id: i64,
    /// The sending computer's name, or its public link id when unnamed.
    pub computer: String,
    /// The sending computer's public link id.
    pub key: String,
    /// The name of the file in this app's delivery folder.
    pub name: String,
    /// The file's size in bytes.
    pub size: u64,
    /// When the arrival was recorded, in seconds since the Unix epoch.
    pub at: f64,
}

/// A room returned by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// The room identifier.
    pub id: String,
    /// The host where the room is homed.
    pub home: String,
    /// The room title.
    pub title: String,
    /// When the room was created, in seconds since the Unix epoch.
    pub created: f64,
    /// The latest message sequence number, or zero for an empty room.
    pub last_seq: u64,
}

/// A room message returned by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// The message sequence number within its room, including logged status changes.
    pub seq: u64,
    /// The message kind as an extensible word.
    pub kind: String,
    /// The host that wrote the message.
    pub author_host: String,
    /// The member who wrote the message.
    pub author: Member,
    /// The message text.
    pub body: String,
    /// When the message was sent, in seconds since the Unix epoch.
    pub sent_at: f64,
    /// When delivery was recorded, in seconds since the Unix epoch, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<f64>,
}

/// A live room status line returned by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusLine {
    /// The status key within its room.
    pub key: String,
    /// The status text.
    pub line: String,
    /// The member who last set this status line.
    pub set_by: Member,
    /// When the status was set, in seconds since the Unix epoch.
    pub set_at: f64,
    /// When the status expires, in seconds since the Unix epoch, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

/// Returns the default socket path beneath the given home directory.
pub fn socket_path(home: &Path) -> PathBuf {
    home.join(".local/state/radio/radio.sock")
}

/// A fleet returned by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    /// The fleet identifier.
    pub id: String,
    /// The fleet name.
    pub name: String,
    /// The signed fleet record's version.
    pub version: u64,
    /// The founding member's public link id.
    pub founder: String,
}

/// An open join code returned without its secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinCodeInfo {
    /// The identifier used to revoke the join code.
    pub id: String,
    /// The optional label given when the code was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the code was made, in seconds since the Unix epoch.
    pub created: f64,
    /// When the code expires, in seconds since the Unix epoch.
    pub expires_at: f64,
    /// The remaining uses of the code.
    pub uses: u32,
}

/// A computer in this computer's view of its fleet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Computer {
    /// The computer's public link id.
    pub id: String,
    /// The computer's name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The computer's state as an extensible word.
    pub state: String,
    /// The inviter's public link id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<String>,
    /// Whether future invitations from this computer are accepted at once.
    pub trusted: bool,
}

/// A computer's signed record, with only the requesting app's section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerRecord {
    /// The computer's public link id.
    pub id: String,
    /// The name the computer signed.
    pub name: String,
    /// The version the computer last signed.
    pub version: u64,
    /// The computer's roles, in name order.
    pub roles: Vec<String>,
    /// The requesting app's section, when the computer holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_address_contract_round_trips() {
        assert_eq!(PROTOCOL_MINOR, 7);
        assert_eq!(
            &OPS[20..23],
            &[
                "link_address_add",
                "link_address_list",
                "link_address_remove"
            ]
        );
        for request in [
            Request::LinkAddressAdd {
                address: "127.0.0.1".into(),
            },
            Request::LinkAddressList {},
            Request::LinkAddressRemove {
                address: "127.0.0.1".into(),
            },
        ] {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(
                serde_json::from_value::<Request>(value.clone()).unwrap(),
                request
            );
            let mut unknown = value;
            unknown["extra"] = true.into();
            assert!(serde_json::from_value::<Request>(unknown).is_err());
        }
        let value = serde_json::json!({"ok":true,"port":7341,"addresses":[],"removed":false});
        let response: Response = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(response.port, Some(7341));
        assert_eq!(response.addresses, Some(vec![]));
        assert_eq!(serde_json::to_value(response).unwrap(), value);
        assert_eq!(
            serde_json::to_value(Response {
                ok: true,
                ..Default::default()
            })
            .unwrap(),
            serde_json::json!({"ok":true})
        );
    }

    #[test]
    fn network_contract_round_trips() {
        assert_eq!(&OPS[23..26], &["join", "computer_remove", "leave"]);
        for request in [
            Request::Join {
                code: "rj1.nope".into(),
            },
            Request::ComputerRemove {
                computer: "birch".into(),
            },
            Request::Leave {},
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            assert_eq!(
                serde_json::from_value::<Request>(value.clone()).unwrap(),
                request
            );
            value["extra"] = true.into();
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        let value = serde_json::json!({"ok":true,"delivered":2});
        let response: Response = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(response.delivered, Some(2));
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"ok":true,"delivered":2}"#
        );
        assert_eq!(serde_json::to_value(response).unwrap(), value);
    }
}
