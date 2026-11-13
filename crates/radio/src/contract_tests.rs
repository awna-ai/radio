use super::tests::{half_close, read_timeout};
use super::*;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

#[path = "../tests/support/contract_match.rs"]
mod contract_match;
use contract_match::contained;

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
    let mut server = Server::bind(&socket, &temp.0.join("rooms.db"), "test-host".into())
        .unwrap()
        .with_link_dir(temp.0.join(".config/radio"))
        .with_fleet_db(temp.0.join("fleet.db"));
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
