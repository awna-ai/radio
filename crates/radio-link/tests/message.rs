mod common;

use common::{Book, SHA, bounded};
use radio_link::{
    EndpointId, SecretKey,
    admit::{Limits, Standing},
    frame::{self, CONTROL_CAP},
    handler::{Envelope, Handlers, Ran},
    link::{BAD_FRAME, Link, Local},
    wire::Answer,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Fake {
    runs: AtomicUsize,
    refused: Mutex<Vec<String>>,
    ids: Mutex<Vec<String>>,
}
impl Handlers for Fake {
    fn has_handler(&self, app: &str) -> bool {
        app == "notes"
    }
    fn run(&self, envelope: &Envelope<'_>) -> Ran {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.ids.lock().unwrap().push(envelope.id.into());
        assert_eq!(envelope.app, "notes");
        Ran::Reply {
            reply: envelope.body.into(),
            exit: 3,
        }
    }
    fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
        self.refused.lock().unwrap().push(reason.into());
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn app(target: &Link, id: &str, sent: u64, body: &str) -> serde_json::Value {
    serde_json::json!({"type":"app", "to":target.id().to_string(), "app":"notes", "id":id, "sent":sent, "body":body})
}
async fn request(
    connection: &iroh::endpoint::Connection,
    value: &serde_json::Value,
) -> Result<Answer, &'static str> {
    bounded(async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(|_| "open failed")?;
        frame::write(&mut send, value, CONTROL_CAP).await?;
        send.finish().map_err(|_| "finish failed")?;
        frame::read(&mut recv, CONTROL_CAP).await
    })
    .await
}
async fn fixture() -> (Link, Link, Arc<Fake>, iroh::endpoint::Connection, Arc<Book>) {
    fixture_limits(Limits::default()).await
}

async fn fixture_limits(
    limits: Limits,
) -> (Link, Link, Arc<Fake>, iroh::endpoint::Connection, Arc<Book>) {
    let book = Book::new();
    let fake = Arc::new(Fake::default());
    let target = bounded(Link::bind(
        SecretKey::generate(),
        &["127.0.0.1:0".parse().unwrap()],
        book.clone(),
        limits,
        Local {
            name: "garden".into(),
            sha: SHA.into(),
            clients: None,
        },
        fake.clone(),
    ))
    .await
    .unwrap();
    let (events, _receiver) = tokio::sync::mpsc::channel(64);
    target.serve(events);
    let sender_book = Book::new();
    let (sender, _events) = common::bind(sender_book.clone(), Limits::default()).await;
    book.set(sender.id(), Standing::Accepted);
    sender_book.set(target.id(), Standing::Accepted);
    let peer = bounded(sender.dial(target.id(), &target.addrs()))
        .await
        .unwrap();
    let connection = peer.connection().clone();
    (target, sender, fake, connection, book)
}

#[tokio::test]
async fn reply_and_exit_round_trip_without_an_app_event() {
    let (target, sender, fake, connection, _book) = fixture().await;
    assert_eq!(
        request(&connection, &app(&target, &"a".repeat(32), now(), "opaque")).await,
        Ok(Answer::Reply {
            reply: "opaque".into(),
            exit: 3
        })
    );
    assert_eq!(fake.runs.load(Ordering::SeqCst), 1);
    assert!(fake.refused.lock().unwrap().is_empty());
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn envelopes_are_refused_before_running_and_each_is_audited() {
    let (target, sender, fake, connection, _book) = fixture().await;
    for (index, delta, body, reason) in [
        (0, -121_000_i64, "x".into(), "clock"),
        (1, 121_000, "x".into(), "clock"),
        (2, 0, "x".repeat(8193), "too_big"),
    ] {
        let value = app(
            &target,
            &format!("{index:032x}"),
            now().saturating_add_signed(delta),
            &body,
        );
        assert_eq!(
            request(&connection, &value).await,
            Ok(Answer::Refused {
                reason: reason.into()
            })
        );
        assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
        assert_eq!(
            fake.refused.lock().unwrap().last().map(String::as_str),
            Some(reason)
        );
    }
    assert_eq!(fake.refused.lock().unwrap().len(), 3);
    for (field, value, reason) in [
        ("id", "A".repeat(32), "bad_frame"),
        (
            "to",
            SecretKey::generate().public().to_string(),
            "not_for_us",
        ),
        ("app", "Notes".into(), "bad_app"),
    ] {
        let mut invalid = app(&target, &"a".repeat(32), now(), "x");
        invalid[field] = serde_json::json!(value);
        let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
        frame::write(&mut send, &invalid, CONTROL_CAP)
            .await
            .unwrap();
        send.finish().unwrap();
        assert!(
            matches!(bounded(recv.read_to_end(CONTROL_CAP)).await, Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code))) if code == BAD_FRAME.into())
        );
        assert_eq!(
            fake.refused.lock().unwrap().last().map(String::as_str),
            Some(reason)
        );
        assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
    }
    assert_eq!(fake.refused.lock().unwrap().len(), 6);
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn replay_and_missing_handler_are_refused_and_audited() {
    let (target, sender, fake, connection, _book) = fixture().await;
    let value = app(&target, &"a".repeat(32), now(), &"x".repeat(8192));
    assert!(matches!(
        request(&connection, &value).await,
        Ok(Answer::Reply { .. })
    ));
    assert_eq!(
        request(&connection, &value).await,
        Ok(Answer::Refused {
            reason: "replay".into()
        })
    );
    let mut missing = app(&target, &"b".repeat(32), now(), "x");
    missing["app"] = serde_json::json!("garden");
    assert_eq!(
        request(&connection, &missing).await,
        Ok(Answer::Refused {
            reason: "no_handler".into()
        })
    );
    assert_eq!(fake.runs.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.refused.lock().unwrap(), ["replay", "no_handler"]);
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn an_honest_sender_makes_fresh_ids_and_receives_typed_replies() {
    let (target, sender, fake, _connection, _book) = fixture().await;
    let peer = bounded(sender.dial(target.id(), &target.addrs()))
        .await
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            bounded(peer.send_app("notes", "opaque")).await,
            Ok(radio_link::handler::Reply {
                reply: "opaque".into(),
                exit: 3
            })
        );
    }
    let ids = fake.ids.lock().unwrap().clone();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }));
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn a_key_no_longer_accepted_is_audited_before_its_frame_runs() {
    let (target, sender, fake, connection, book) = fixture().await;
    book.set(sender.id(), Standing::Refused);
    assert!(
        request(&connection, &app(&target, &"a".repeat(32), now(), "opaque"))
            .await
            .is_err()
    );
    common::closed(&connection, radio_link::link::REFUSED).await;
    assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
    assert_eq!(*fake.refused.lock().unwrap(), ["not_member"]);
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn an_app_frame_without_fin_keeps_the_short_frame_deadline() {
    let (target, sender, fake, connection, _book) = fixture_limits(Limits {
        frame: std::time::Duration::from_millis(100),
        handler: std::time::Duration::from_secs(2),
        ..Limits::default()
    })
    .await;
    let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
    frame::write(
        &mut send,
        &app(&target, &"a".repeat(32), now(), "opaque"),
        CONTROL_CAP,
    )
    .await
    .unwrap();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        recv.read_to_end(CONTROL_CAP),
    )
    .await;
    assert!(
        matches!(read, Ok(Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code)))) if code == BAD_FRAME.into())
    );
    assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
    bounded(target.close()).await;
    bounded(sender.close()).await;
}

#[tokio::test]
async fn a_nonmember_handshake_is_audited_without_running_a_handler() {
    let (target, sender, fake, _connection, _book) = fixture().await;
    let raw_book = Book::new();
    raw_book.set(target.id(), Standing::Accepted);
    let raw = common::raw_bind(raw_book, Limits::default()).await;
    let connection = raw.connect(&target, radio_link::endpoint::ALPN).await;
    common::closed(&connection, radio_link::link::REFUSED).await;
    assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
    assert_eq!(*fake.refused.lock().unwrap(), ["not_member"]);
    bounded(target.close()).await;
    bounded(sender.close()).await;
    bounded(raw.close()).await;
}
