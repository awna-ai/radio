use super::tests::{half_close, read_timeout};
use super::*;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

fn contained(recorded: &Value, actual: &Value, key: &str) -> bool {
    match (recorded, actual) {
        (Value::String(s), Value::Number(n)) if s == "<time>" => n
            .as_f64()
            .is_some_and(|time| time.is_finite() && (1e9..=1e11).contains(&time)),
        (Value::String(s), Value::String(_)) if s == "<string>" => true,
        (Value::Object(expected), Value::Object(got)) => expected.iter().all(|(key, value)| {
            got.get(key)
                .is_some_and(|actual| contained(value, actual, key))
        }),
        (Value::Array(expected), Value::Array(got)) if ["ops", "majors"].contains(&key) => {
            expected.iter().all(|value| got.contains(value))
        }
        (Value::Array(expected), Value::Array(got)) => {
            expected.len() == got.len()
                && expected
                    .iter()
                    .zip(got)
                    .all(|(value, actual)| contained(value, actual, ""))
        }
        _ => recorded == actual,
    }
}

#[test]
fn matching_is_additive_but_preserves_order_values_and_types() {
    use serde_json::json;
    assert!(contained(
        &json!({"a":"<time>","b":"<string>","ops":["hello"],"majors":[1]}),
        &json!({"a":1.7e9,"b":"ok","ops":["new","hello"],"majors":[2,1],"extra":true}),
        ""
    ));
    for (recorded, actual) in [
        (json!({"a":1}), json!({"b":1})),
        (json!([1, 2]), json!([2, 1])),
        (json!([1]), json!([1, 2])),
        (json!("<time>"), json!("1")),
        (json!("<time>"), json!(1.7e12)),
        (json!("<time>"), json!(0)),
        (json!("<time>"), json!(999999999)),
        (json!("<time>"), json!(100000000001_u64)),
        (json!("<string>"), json!(1)),
        (json!(true), json!(false)),
        (json!({"ops":["hello"]}), json!({"ops":[]})),
        (json!({"majors":[1]}), json!({"majors":[2]})),
    ] {
        assert!(!contained(&recorded, &actual, ""), "{recorded} {actual}");
    }
}

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn replay_major_one_contract() {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/v1"));
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no contract fixtures");
    let fixtures: Vec<Value> = paths
        .iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), 3, "{}", path.display());
            assert!(
                object.contains_key("request")
                    && object.contains_key("response")
                    && object.get("exact").unwrap().is_boolean()
            );
            value
        })
        .collect();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = Temp(PathBuf::from(format!(
        "/tmp/rd-contract-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    let socket = temp.0.join("radio.sock");
    let mut server = Server::bind(&socket, &temp.0.join("rooms.db"), "test-host".into()).unwrap();
    let connections = fixtures.len();
    let worker = thread::spawn(move || {
        for _ in 0..connections {
            let (stream, _) = server.listener.accept().unwrap();
            server.serve_one(stream);
        }
    });
    for (fixture, path) in fixtures.iter().zip(paths) {
        let mut stream = UnixStream::connect(&socket).unwrap();
        read_timeout(&stream, Duration::from_secs(5));
        // The recorded request, sent compact.
        serde_json::to_writer(&mut stream, &fixture["request"]).unwrap();
        stream.write_all(b"\n").unwrap();
        half_close(&stream);
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let actual: Value = serde_json::from_str(&line).unwrap();
        if fixture["exact"] == true {
            assert_eq!(fixture["response"], actual, "{}", path.display());
        } else {
            assert!(
                contained(&fixture["response"], &actual, ""),
                "{}: expected {} in {}",
                path.display(),
                fixture["response"],
                actual
            );
        }
    }
    worker.join().unwrap();
}
