use super::*;
use serde_json::json;

#[test]
fn an_answer_sent_before_the_close_is_read() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":false,\"error\":\"refused: another user\",\"refused\":true}\n")
        .unwrap();
    drop(peer);

    let response = exchange(stream, &json!({"v":1,"op":"hello"}), TIMEOUT);
    assert!(response.is_ok(), "{response:?}");
    let (response, _) = response.unwrap();
    assert_eq!(response.refused, Some(true));
    assert_eq!(response.error.as_deref(), Some("refused: another user"));
}

#[test]
fn a_close_without_an_answer_is_no_answer() {
    let (stream, peer) = UnixStream::pair().unwrap();
    drop(peer);

    assert_eq!(
        exchange(stream, &json!({"v":1,"op":"hello"}), TIMEOUT).unwrap_err(),
        NO_ANSWER
    );
}

#[test]
fn a_partial_answer_before_the_close_is_no_answer() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":true").unwrap();
    drop(peer);

    assert_eq!(
        exchange(stream, &json!({"v":1,"op":"hello"}), TIMEOUT).unwrap_err(),
        NO_ANSWER
    );
}

fn answer_from_listener(answer: Vec<u8>) -> Result<(Response, String), String> {
    use std::{
        fs,
        os::unix::net::UnixListener,
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let socket = std::env::temp_dir().join(format!(
        "rd-line-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&socket).unwrap();
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&peer).read_line(&mut request).unwrap();
        peer.write_all(&answer).unwrap();
    });
    let result = request_line(&socket, &Request::Hello {});
    worker.join().unwrap();
    fs::remove_file(socket).unwrap();
    result
}

#[test]
fn answer_line_preserves_unknown_fields_spacing_and_order() {
    let answer = "{\"ok\":true,  \"later\":{\"b\":1,\"a\":2}}";
    let (response, line) = answer_from_listener(format!("{answer}\n").into_bytes()).unwrap();
    assert!(response.ok);
    assert_eq!(line, answer);
}

#[test]
fn invalid_utf8_answer_is_no_answer() {
    assert_eq!(
        answer_from_listener(b"{\"ok\":true,\"later\":\"\xff\"}\n".to_vec()).unwrap_err(),
        NO_ANSWER
    );
}

#[test]
fn buffered_answer_bytes_and_encoding_are_preserved() {
    let answer = b"{\"ok\":true,  \"later\":{\"b\":1,\"a\":2}}\n";
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(answer).unwrap();
    drop(peer);
    let (response, line) = exchange(stream, &json!({"v":1,"op":"hello"}), TIMEOUT).unwrap();
    assert!(response.ok);
    assert_eq!(line.as_bytes(), &answer[..answer.len() - 1]);

    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":true,\"later\":\"\xff\"}\n")
        .unwrap();
    drop(peer);
    assert_eq!(
        exchange(stream, &json!({"v":1,"op":"hello"}), TIMEOUT).unwrap_err(),
        NO_ANSWER
    );
}

#[test]
fn network_ops_wait_for_deliveries() {
    for request in [
        Request::Join {
            code: "rj1.nope".into(),
        },
        Request::ComputerRemove {
            computer: "birch".into(),
        },
        Request::Leave {},
    ] {
        assert_eq!(answer_wait(&request), Duration::from_secs(120));
    }
    for request in [
        Request::Hello {},
        Request::ComputerBlock {
            computer: "birch".into(),
        },
        Request::Resign {
            app: "lantern".into(),
            section: None,
            roles: None,
        },
    ] {
        assert_eq!(answer_wait(&request), Duration::from_secs(5));
    }
}

#[test]
fn file_sends_wait_for_the_node_deadline() {
    let request: Request = serde_json::from_value(
        json!({"op":"send","app":"notes","computer":"garden","name":"letter","file":"/letter"}),
    )
    .unwrap();
    assert_eq!(answer_wait(&request), Duration::from_secs(135));
}
