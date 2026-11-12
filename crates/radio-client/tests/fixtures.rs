use radio_client::{Request, Response};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn placeholders(value: &mut Value) {
    match value {
        Value::String(s) if s == "<time>" => *value = json!(1.7e9),
        Value::String(s) if s == "<string>" => *value = json!(""),
        Value::Object(map) => {
            for value in map.values_mut() {
                placeholders(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                placeholders(value);
            }
        }
        _ => {}
    }
}

#[test]
fn recorded_contract_uses_client_types() {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/v1"));
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no contract fixtures");
    let count = paths.len();
    let mut requests = 0;
    for path in paths {
        let mut fixture: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let object = fixture.as_object().unwrap();
        assert_eq!(object.len(), 3, "{}", path.display());
        assert!(
            object.contains_key("request")
                && object.contains_key("response")
                && object.get("exact").unwrap().is_boolean()
        );
        let mut expected = fixture["request"].clone();
        let version = expected.as_object_mut().unwrap().remove("v").unwrap();
        assert!(version.is_u64());
        let request: Request = serde_json::from_value(expected).unwrap();
        let mut recorded = serde_json::to_value(request).unwrap();
        recorded
            .as_object_mut()
            .unwrap()
            .insert("v".into(), version);
        assert_eq!(recorded, fixture["request"], "{}", path.display());
        requests += 1;
        placeholders(&mut fixture["response"]);
        let _: Response = serde_json::from_value(fixture["response"].take()).unwrap();
    }
    assert_eq!(requests, count, "every fixture request must round-trip");
}

#[test]
fn fixtures_record_refusals_optional_fields_and_duplicate_rooms() {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/v1"));
    let fixtures: Vec<Value> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect();
    assert!(fixtures.iter().any(|f| f["request"]["op"] == "post"
        && f["request"]["author"] == json!({"kind":"scribe","name":"wren"})
        && f["response"] == json!({"ok":false,"error":"not a member","refused":true})));
    assert!(fixtures.iter().any(|f| f["request"]["op"] == "status_set"
        && f["request"]["key"] == "plan"
        && f["request"]["expires_at"] == json!(4102444800.0)));
    assert!(fixtures.iter().any(|f| f["request"]["op"] == "read"
        && f["request"]["since"].is_number()
        && f["request"]["limit"].is_number()));
    for field in ["limit", "after"] {
        assert!(
            fixtures
                .iter()
                .any(|f| f["request"]["op"] == "list_rooms" && f["request"].get(field).is_some()),
            "missing list_rooms {field}"
        );
    }
    assert!(fixtures.iter().any(|f| f["request"]["op"] == "create_room"
        && f["response"] == json!({"ok":false,"error":"the room already exists"})));
}
