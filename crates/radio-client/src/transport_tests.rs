use super::*;
use serde_json::json;

#[test]
fn an_answer_sent_before_the_close_is_read() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":false,\"error\":\"refused: another user\",\"refused\":true}\n")
        .unwrap();
    drop(peer);

    let response = exchange(stream, &json!({"v":1,"op":"hello"}));
    assert!(response.is_ok(), "{response:?}");
    let response = response.unwrap();
    assert_eq!(response.refused, Some(true));
    assert_eq!(response.error.as_deref(), Some("refused: another user"));
}

#[test]
fn a_close_without_an_answer_is_no_answer() {
    let (stream, peer) = UnixStream::pair().unwrap();
    drop(peer);

    assert_eq!(
        exchange(stream, &json!({"v":1,"op":"hello"})).unwrap_err(),
        "the Radio did not answer"
    );
}

#[test]
fn a_partial_answer_before_the_close_is_no_answer() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":true").unwrap();
    drop(peer);

    assert_eq!(
        exchange(stream, &json!({"v":1,"op":"hello"})).unwrap_err(),
        "the Radio did not answer"
    );
}
