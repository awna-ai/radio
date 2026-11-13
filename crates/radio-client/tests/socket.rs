use radio_client::*;
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::Shutdown,
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

struct Socket(PathBuf);
impl Socket {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(format!(
            "/tmp/rd-client-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("radio.sock")
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn default_path() {
    assert_eq!(
        std::panic::catch_unwind(|| socket_path(std::path::Path::new("/tmp/home"))).ok(),
        Some(PathBuf::from("/tmp/home/.local/state/radio/radio.sock"))
    );
}

#[test]
fn forward_fields_and_message_kinds_are_readable() {
    let response: Response = serde_json::from_value(json!({"ok":true,"future":1,"protocol":{"major":1,"minor":0,"future":1},"rooms":[{"id":"garden","home":"test-host","title":"Garden","created":0.0,"last_seq":1,"future":1}],"messages":[{"seq":1,"kind":"future_kind","author_host":"test-host","author":{"kind":"helper","name":"juniper"},"body":"hi","sent_at":0.0,"future":1}],"status":[{"key":"plan","line":"Ready","set_by":{"kind":"human","name":"sam"},"set_at":0.0,"future":1}]})).unwrap();
    assert_eq!(response.messages.unwrap()[0].kind, "future_kind");
    assert_eq!(
        serde_json::to_value(Response {
            ok: true,
            ..Default::default()
        })
        .unwrap(),
        json!({"ok":true})
    );
}

#[test]
fn empty_ops_and_members_refuse_unknown_fields() {
    for value in [
        json!({"op":"hello","x":1}),
        json!({"op":"list_rooms","x":1}),
        json!({"op":"register","app":"lantern","app_version":"1","members":[{"kind":"helper","name":"juniper","x":1}]}),
    ] {
        assert!(serde_json::from_value::<Request>(value).is_err());
    }
}

#[test]
fn request_inserts_version_writes_line_and_half_closes() {
    let socket = Socket::new();
    let listener = UnixListener::bind(socket.path()).unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            json!({"v":1,"op":"hello"})
        );
        line.clear();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        stream.write_all(b"{\"ok\":true}\n").unwrap();
    });
    assert!(request(&socket.path(), &Request::Hello {}).unwrap().ok);
    worker.join().unwrap();
}

#[test]
fn response_framing_and_cap() {
    for (bytes, valid) in [
        (b"{\"ok\":true}\nsecond\n".to_vec(), true),
        (b"{\"ok\":true}".to_vec(), false),
        (b"not json\n".to_vec(), false),
        (
            {
                let mut b = b"{\"ok\":true}".to_vec();
                b.resize(RESPONSE_MAX_BYTES - 1, b' ');
                b.push(b'\n');
                b
            },
            true,
        ),
        (
            {
                let mut b = b"{\"ok\":true}".to_vec();
                b.resize(RESPONSE_MAX_BYTES, b' ');
                b.push(b'\n');
                b
            },
            false,
        ),
    ] {
        let socket = Socket::new();
        let listener = UnixListener::bind(socket.path()).unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = stream.write_all(&bytes);
        });
        let result = request(&socket.path(), &Request::Hello {});
        if valid {
            assert!(result.unwrap().ok)
        } else {
            assert_eq!(result.unwrap_err(), "Radio did not answer")
        }
        worker.join().unwrap();
    }
}

#[test]
fn closed_peer_can_still_answer_and_missing_server_has_fixed_error() {
    let socket = Socket::new();
    assert_eq!(
        request(&socket.path(), &Request::Hello {}).unwrap_err(),
        "Radio is not running"
    );
    let listener = UnixListener::bind(socket.path()).unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(b"{\"ok\":false,\"error\":\"refused: another user\",\"refused\":true}\n")
            .unwrap();
        let _ = stream.shutdown(Shutdown::Both);
    });
    let response = request(&socket.path(), &Request::Hello {}).unwrap();
    assert_eq!(response.refused, Some(true));
    worker.join().unwrap();
}
