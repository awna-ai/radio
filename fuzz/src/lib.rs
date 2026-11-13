use radio_link::{
    code::JoinCode,
    computer_record::ComputerRecord,
    fleet::{self, FleetRecord},
    frame as framing,
    membership::{JoinAnswer, SignedRecord},
    wire::{Answer, JoinRequest, Request},
};
use std::sync::OnceLock;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("the in-memory runtime builds")
    })
}

pub fn frame(data: &[u8]) -> u32 {
    runtime().block_on(async {
        macro_rules! decode {
            ($kind:ty, $cap:expr) => {{
                if let Ok(value) = framing::read::<$kind, _>(&mut &data[..], $cap).await {
                    let mut encoded = Vec::new();
                    let length = serde_json::to_vec(&value)
                        .expect("a decoded frame serializes")
                        .len();
                    let written = framing::write(&mut encoded, &value, $cap).await;
                    if length > $cap {
                        assert_eq!(written, Err("the frame is too large"));
                        assert!(encoded.is_empty());
                    } else {
                        written.expect("a decoded frame within the cap writes again");
                        let decoded = framing::read::<$kind, _>(&mut &encoded[..], $cap)
                            .await
                            .expect("a written frame reads again");
                        assert!(decoded == value);
                    }
                    Some(value)
                } else {
                    None
                }
            }};
        }
        let mut checked = 0;
        if let Some(request) = decode!(Request, framing::CONTROL_CAP) {
            checked += u32::from(match request {
                Request::Computer { record } => record.verify().is_ok(),
                Request::Record { record } => record.verify().is_ok(),
                Request::Hello { .. } | Request::App { .. } => true,
            });
        }
        if decode!(Answer, framing::CONTROL_CAP).is_some() {
            checked += 1;
        }
        if let Some(request) = decode!(JoinRequest, framing::CONTROL_CAP) {
            checked += u32::from(JoinCode::parse(&request.code).is_ok());
        }
        if let Some(answer) = decode!(JoinAnswer, framing::RECORD_CAP) {
            checked += u32::from(fleet::accept(None, &answer.fleet).is_ok());
            for record in answer.records {
                checked += u32::from(record.verify().is_ok());
            }
        }
        checked
    })
}

pub fn records(data: &[u8]) -> u32 {
    macro_rules! check {
        ($kind:ty) => {{
            if let Ok(record) = serde_json::from_slice::<$kind>(data) {
                if record.verify().is_ok() {
                    let encoded = serde_json::to_vec(&record).expect("a record serializes");
                    let decoded = serde_json::from_slice::<$kind>(&encoded)
                        .expect("a serialized record decodes");
                    assert!(decoded == record);
                    assert!(decoded.verify().is_ok());
                    1
                } else {
                    0
                }
            } else {
                0
            }
        }};
    }
    check!(ComputerRecord) + check!(SignedRecord) + check!(FleetRecord)
}

pub fn join_code(data: &[u8]) -> u32 {
    let Ok(text) = std::str::from_utf8(data) else {
        return 0;
    };
    let Ok(code) = JoinCode::parse(text) else {
        return 0;
    };
    let decoded = JoinCode::parse(&code.encode()).expect("an encoded code parses");
    assert!(decoded == code);
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use radio_link::{
        SecretKey,
        code::{Hints, JoinCode},
        computer_record::{self, ComputerBody},
        fleet,
        membership::{self, JoinAnswer, RecordBody},
        wire::{Answer, Hello, JoinRequest, Protocol, Request},
    };
    use std::collections::BTreeMap;

    fn node() -> SecretKey {
        SecretKey::from_bytes(&[1; 32])
    }
    fn computer() -> computer_record::ComputerRecord {
        let node = node();
        computer_record::sign(
            ComputerBody {
                fleet: "ab".repeat(32),
                key: node.public().to_string(),
                version: 1,
                name: "garden".into(),
                roles: vec![],
                addrs: vec![],
                apps: BTreeMap::new(),
            },
            &node,
        )
    }
    fn code() -> JoinCode {
        JoinCode::new("ab".repeat(32), node().public(), Hints::default()).unwrap()
    }
    fn hello() -> Hello {
        Hello {
            protocol: Protocol { major: 1, minor: 2 },
            fleet: "ab".repeat(32),
            sha: "cd".repeat(32),
            arch: "aarch64".into(),
            name: "garden".into(),
        }
    }
    fn framed(json: Vec<u8>) -> Vec<u8> {
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        bytes
    }
    #[test]
    fn frame_checks_valid_inputs() {
        for json in [
            serde_json::to_vec(&Request::Computer { record: computer() }).unwrap(),
            serde_json::to_vec(&Request::Hello { hello: hello() }).unwrap(),
            serde_json::to_vec(&Answer::Ok).unwrap(),
            serde_json::to_vec(&JoinRequest {
                code: code().encode(),
                consent: String::new(),
                name: "birch".into(),
                addrs: vec![],
            })
            .unwrap(),
            serde_json::to_vec(&JoinAnswer {
                fleet: fleet::found(&node(), "garden").unwrap().0,
                computers: vec![],
                removed: vec![],
                records: vec![computer()],
            })
            .unwrap(),
        ] {
            assert!(frame(&framed(json)) >= 1);
        }
    }
    #[test]
    fn a_frame_that_grows_on_rewrite_is_not_a_crash() {
        let json = r#"{"code":"garden","consent":"","name":"birch","addrs":["[::ffff:a:a]:1","[1:2:3:4:5:6::8]:1"]}"#;
        let padding = "a".repeat(framing::CONTROL_CAP - json.len());
        let json = json.replace(r#""consent":"""#, &format!(r#""consent":"{padding}""#));
        let request: JoinRequest = serde_json::from_str(&json).unwrap();
        assert!(json.len() <= framing::CONTROL_CAP);
        assert!(serde_json::to_vec(&request).unwrap().len() > framing::CONTROL_CAP);
        assert_eq!(frame(&framed(json.into_bytes())), 0);
    }
    #[test]
    fn records_checks_each_valid_kind() {
        let node = node();
        let newcomer = SecretKey::from_bytes(&[2; 32]);
        let fleet_id = "ab".repeat(32);
        let addrs = vec!["127.0.0.1:7000".parse().unwrap()];
        let introduction = membership::sign(
            RecordBody::Introduction {
                fleet: fleet_id.clone(),
                inviter: node.public().to_string(),
                counter: 1,
                newcomer: newcomer.public().to_string(),
                name: "birch".into(),
                consent: membership::consent(
                    &newcomer,
                    &fleet_id,
                    &node.public().to_string(),
                    "birch",
                    &addrs,
                ),
                addrs,
            },
            &node,
        );
        let removal = membership::sign(
            RecordBody::Removal {
                fleet: fleet_id,
                signer: node.public().to_string(),
                counter: 2,
                subject: newcomer.public().to_string(),
            },
            &node,
        );
        let (found, recovery) = fleet::found(&node, "garden").unwrap();
        let recovered = fleet::sign(found.body.clone(), fleet::Signer::Recovery, &recovery);
        for json in [
            serde_json::to_vec(&computer()).unwrap(),
            serde_json::to_vec(&introduction).unwrap(),
            serde_json::to_vec(&removal).unwrap(),
            serde_json::to_vec(&found).unwrap(),
            serde_json::to_vec(&recovered).unwrap(),
        ] {
            assert!(records(&json) >= 1);
        }
    }
    #[test]
    fn join_code_checks_valid_input() {
        assert!(join_code(code().encode().as_bytes()) >= 1);
    }
    #[test]
    fn decoded_values_with_failed_checks_do_not_count() {
        let mut record = computer();
        record.body.version += 1;
        assert_eq!(records(&serde_json::to_vec(&record).unwrap()), 0);
        assert_eq!(
            frame(&framed(
                serde_json::to_vec(&Request::Computer {
                    record: record.clone()
                })
                .unwrap()
            )),
            0
        );
        assert_eq!(
            frame(&framed(
                serde_json::to_vec(&JoinRequest {
                    code: "garbage".into(),
                    consent: String::new(),
                    name: "birch".into(),
                    addrs: vec![]
                })
                .unwrap()
            )),
            0
        );
        let mut answer = JoinAnswer {
            fleet: fleet::found(&node(), "garden").unwrap().0,
            computers: vec![],
            removed: vec![],
            records: vec![record],
        };
        assert_eq!(frame(&framed(serde_json::to_vec(&answer).unwrap())), 1);
        answer.fleet.body.version += 1;
        assert_eq!(frame(&framed(serde_json::to_vec(&answer).unwrap())), 0);
    }
    #[test]
    fn invalid_inputs_pass_no_checks() {
        for data in [
            vec![],
            b"garbage".to_vec(),
            ((radio_link::frame::RECORD_CAP + 1) as u32)
                .to_be_bytes()
                .to_vec(),
        ] {
            assert_eq!(frame(&data), 0);
            assert_eq!(records(&data), 0);
            assert_eq!(join_code(&data), 0);
        }
    }
}
