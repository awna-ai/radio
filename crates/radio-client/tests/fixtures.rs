use radio_client::{Request, Response};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn placeholders(value: &mut Value) {
    match value {
        Value::String(s) if s == "<time>" => *value = json!(0.0),
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
    for path in paths {
        let mut fixture: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let object = fixture.as_object().unwrap();
        assert_eq!(object.len(), 3, "{}", path.display());
        assert!(
            object.contains_key("request")
                && object.contains_key("response")
                && object.get("exact").unwrap().is_boolean()
        );
        if fixture["exact"] == true {
            continue;
        }
        let mut expected = fixture["request"].clone();
        assert_eq!(
            expected.as_object_mut().unwrap().remove("v"),
            Some(json!(1))
        );
        let request: Request = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            expected,
            "{}",
            path.display()
        );
        placeholders(&mut fixture["response"]);
        let _: Response = serde_json::from_value(fixture["response"].take()).unwrap();
    }
}
