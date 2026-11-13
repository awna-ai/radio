mod common;

use common::*;
use radio_link::{
    SecretKey,
    admit::{Admission, Limits, Standing},
    endpoint::{ALPN, JOIN_ALPN},
    frame::{self, CONTROL_CAP},
    link::{BAD_FRAME, Event, IDLE, REFUSED},
    wire::{Answer, Request},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, timeout};

async fn pair(
    limits: Limits,
) -> (
    radio_link::link::Link,
    radio_link::link::Link,
    Arc<Book>,
    tokio::sync::mpsc::Receiver<Event>,
) {
    let a_book = Book::new();
    let b_book = Book::new();
    let (a, events) = bind(a_book.clone(), limits).await;
    let (b, _) = bind(b_book.clone(), Limits::default()).await;
    a_book.set(b.id(), Standing::Accepted);
    b_book.set(a.id(), Standing::Accepted);
    (a, b, a_book, events)
}

async fn raw(
    client: &radio_link::link::Link,
    server: &radio_link::link::Link,
    alpn: &[u8],
) -> iroh::endpoint::Connection {
    assert_eq!(alpn, ALPN);
    bounded(client.dial(server.id(), &server.addrs()))
        .await
        .unwrap()
        .connection()
        .clone()
}

async fn request(
    connection: &iroh::endpoint::Connection,
    request: &Request,
) -> Result<Answer, &'static str> {
    bounded(async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(|_| "open failed")?;
        frame::write(&mut send, request, CONTROL_CAP).await?;
        send.finish().map_err(|_| "finish failed")?;
        frame::read(&mut recv, CONTROL_CAP).await
    })
    .await
}

async fn reset(connection: &iroh::endpoint::Connection, bytes: &[u8]) {
    let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
    bounded(send.write_all(bytes)).await.unwrap();
    send.finish().unwrap();
    match bounded(recv.read_to_end(CONTROL_CAP)).await {
        Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code))) => {
            assert_eq!(code, BAD_FRAME.into())
        }
        other => panic!("expected a bad-frame reset, got {other:?}"),
    }
}

fn bytes(request: &Request) -> Vec<u8> {
    let body = serde_json::to_vec(request).unwrap();
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend(body);
    bytes
}

#[tokio::test]
async fn hello_and_app_preserve_authenticated_sender_and_opaque_body() {
    let a_book = Book::new();
    let b_book = Book::new();
    let (a, mut events) = bind(a_book.clone(), Limits::default()).await;
    let (b, mut b_events) = bind(b_book.clone(), Limits::default()).await;
    a_book.set(b.id(), Standing::Accepted);
    b_book.set(a.id(), Standing::Accepted);
    let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
    let answer = bounded(peer.hello(&hello())).await.unwrap();
    assert_eq!(answer.name, "garden");
    assert_eq!(answer.sha, SHA);
    assert_eq!(answer.fleet, FLEET);
    assert_eq!(answer.arch, std::env::consts::ARCH);
    assert_eq!((answer.protocol.major, answer.protocol.minor), (1, 2));
    match bounded(events.recv()).await.unwrap() {
        Event::Hello {
            from,
            hello: received,
        } => {
            assert_eq!(from, b.id());
            assert_eq!(received.name, "juniper");
            assert_eq!(received.fleet, FLEET);
            assert_eq!(received.sha, SHA);
            assert_eq!(received.arch, "aarch64");
            assert_eq!((received.protocol.major, received.protocol.minor), (1, 0));
        }
        _ => panic!("expected hello"),
    }
    let body = "{\"unparsed\":[\"opaque\",42]}";
    bounded(peer.send_app("lantern", body)).await.unwrap();
    match bounded(events.recv()).await.unwrap() {
        Event::App {
            from,
            app,
            body: received,
        } => {
            assert_eq!(from, b.id());
            assert_eq!(app, "lantern");
            assert_eq!(received, body);
        }
        _ => panic!("expected app"),
    }
    let reverse = bounded(a.dial(b.id(), &b.addrs())).await.unwrap();
    assert_eq!(
        bounded(reverse.hello(&hello())).await.unwrap().name,
        "garden"
    );
    match bounded(b_events.recv()).await.unwrap() {
        Event::Hello {
            from,
            hello: received,
        } => {
            assert_eq!(from, a.id());
            assert_eq!(received.name, "juniper");
            assert_eq!(received.fleet, FLEET);
        }
        _ => panic!("expected hello"),
    }
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn invalid_app_destinations_and_names_reset_without_events() {
    let (a, b, _, mut events) = pair(Limits::default()).await;
    let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
    for (to, app) in [
        (SecretKey::generate().public().to_string(), "lantern"),
        (a.id().to_string(), "Lantern"),
    ] {
        reset(
            peer.connection(),
            &bytes(&Request::App {
                to,
                app: app.into(),
                body: "opaque".into(),
            }),
        )
        .await;
        empty(&mut events).await;
    }
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn hook_refuses_unknown_and_removed_keys_with_empty_reasons() {
    for (standing, alpn, join_open) in [
        (Standing::Unknown, ALPN, true),
        (Standing::Refused, ALPN, true),
        (Standing::Refused, JOIN_ALPN, true),
        (Standing::Unknown, JOIN_ALPN, false),
    ] {
        let a_book = Book::new();
        a_book.set_join(join_open);
        let b_book = Book::new();
        let a = bounded(radio_link::link::Link::bind(
            SecretKey::generate(),
            &["127.0.0.1:0".parse().unwrap()],
            a_book.clone(),
            Limits::default(),
            radio_link::link::Local {
                name: "garden".into(),
                sha: SHA.into(),
            },
        ))
        .await
        .unwrap();
        // Keep the join hand-out blocked so only the hook can refuse within 2 s.
        let (sender, mut events) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Event::Hello {
                from: a.id(),
                hello: hello(),
            })
            .unwrap();
        a.serve(sender);
        let b = raw_bind(b_book.clone(), Limits::default()).await;
        a_book.set(b.id(), standing);
        b_book.set(a.id(), Standing::Accepted);
        let connection = b.connect(&a, alpn).await;
        timeout(Duration::from_secs(2), closed(&connection, REFUSED))
            .await
            .unwrap();
        match bounded(events.recv()).await.unwrap() {
            Event::Hello { from, .. } => assert_eq!(from, a.id()),
            _ => panic!("expected filler event"),
        }
        empty(&mut events).await;
        bounded(a.close()).await;
        bounded(b.close()).await;
    }
}

#[tokio::test]
async fn refused_hello_sends_no_event() {
    let a_book = Book::new();
    let b_book = Book::new();
    let (a, mut events) = bind(a_book.clone(), Limits::default()).await;
    let b = raw_bind(b_book.clone(), Limits::default()).await;
    a_book.set(b.id(), Standing::Refused);
    b_book.set(a.id(), Standing::Accepted);
    let connection = b.connect(&a, ALPN).await;
    if let Ok((mut send, _)) = bounded(connection.open_bi()).await {
        let _ = bounded(frame::write(
            &mut send,
            &Request::Hello { hello: hello() },
            CONTROL_CAP,
        ))
        .await;
        let _ = send.finish();
    }
    closed(&connection, REFUSED).await;
    empty(&mut events).await;
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn dialer_without_an_open_join_receives_admission_from_inviter() {
    let book = Book::new();
    book.set_join(true);
    let (a, mut events) = bind(book, Limits::default()).await;
    let client_book = Book::new();
    assert!(!client_book.join_open());
    let b = raw_bind(client_book, Limits::default()).await;
    let connection = b.connect(&a, JOIN_ALPN).await;
    match bounded(events.recv()).await.unwrap() {
        Event::Join {
            from,
            connection: accepted,
        } => {
            assert_eq!(from, b.id());
            assert_eq!(accepted.remote_id(), b.id());
            assert!(connection.close_reason().is_none());
        }
        _ => panic!("expected join"),
    }
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn join_is_admitted_only_while_open() {
    let a_book = Book::new();
    let b_book = Book::new();
    let (a, mut events) = bind(a_book.clone(), Limits::default()).await;
    let b = raw_bind(b_book.clone(), Limits::default()).await;
    b_book.set(a.id(), Standing::Accepted);
    closed(&b.connect(&a, JOIN_ALPN).await, REFUSED).await;
    empty(&mut events).await;
    a_book.set_join(true);
    let connection = b.connect(&a, JOIN_ALPN).await;
    match bounded(events.recv()).await.unwrap() {
        Event::Join {
            from,
            connection: accepted,
        } => {
            assert_eq!(from, b.id());
            assert_eq!(accepted.remote_id(), b.id());
            assert!(connection.close_reason().is_none());
        }
        _ => panic!("expected join"),
    }
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn dials_pin_the_requested_key_and_refuse_unaccepted_keys_locally() {
    let a_book = Book::new();
    let b_book = Book::new();
    let c_book = Book::new();
    let (a, _) = bind(a_book.clone(), Limits::default()).await;
    let (b, _) = bind(b_book.clone(), Limits::default()).await;
    let (c, mut events) = bind(c_book.clone(), Limits::default()).await;
    a_book.set(b.id(), Standing::Accepted);
    b_book.set(a.id(), Standing::Accepted);
    b_book.set(c.id(), Standing::Accepted);
    c_book.set(b.id(), Standing::Accepted);
    let control = bounded(b.dial(c.id(), &c.addrs())).await.unwrap();
    assert_eq!(
        bounded(control.hello(&hello())).await.unwrap().name,
        "garden"
    );
    match bounded(events.recv()).await.unwrap() {
        Event::Hello { from, .. } => assert_eq!(from, b.id()),
        _ => panic!("expected hello"),
    }
    let started = Instant::now();
    let result = timeout(Duration::from_secs(6), b.dial(a.id(), &c.addrs()))
        .await
        .unwrap();
    assert_eq!(result.err(), Some("the computer could not be reached"));
    assert!(started.elapsed() < Duration::from_secs(6));
    empty(&mut events).await;
    for standing in [Standing::Unknown, Standing::Refused] {
        a_book.set(b.id(), standing);
        assert_eq!(
            bounded(a.dial(b.id(), &b.addrs())).await.err(),
            Some("the computer is not let in")
        );
    }
    bounded(a.close()).await;
    bounded(b.close()).await;
    bounded(c.close()).await;
}

#[tokio::test]
async fn caps_per_key_and_total_connections_and_reuses_closed_slots() {
    let a_book = Book::new();
    let (a, mut events) = bind(a_book.clone(), Limits::default()).await;
    let mut clients = Vec::new();
    let mut connections = Vec::new();
    for _ in 0..9 {
        let book = Book::new();
        book.set(a.id(), Standing::Accepted);
        let client = raw_bind(
            book,
            Limits {
                per_key: 40,
                connections: 40,
                ..Limits::default()
            },
        )
        .await;
        a_book.set(client.id(), Standing::Accepted);
        clients.push(client);
    }
    for client in &clients[..8] {
        for _ in 0..4 {
            let connection = client.connect(&a, ALPN).await;
            assert!(matches!(
                request(&connection, &Request::Hello { hello: hello() })
                    .await
                    .unwrap(),
                Answer::Hello { .. }
            ));
            bounded(events.recv()).await.unwrap();
            connections.push(connection);
        }
        closed(&client.connect(&a, ALPN).await, REFUSED).await;
    }
    closed(&clients[8].connect(&a, ALPN).await, REFUSED).await;
    connections[0].close(0u32.into(), &[]);
    bounded(async {
        loop {
            let connection = clients[8].connect(&a, ALPN).await;
            if request(&connection, &Request::Hello { hello: hello() })
                .await
                .is_ok()
            {
                connections.push(connection);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    bounded(a.close()).await;
    for client in clients {
        bounded(client.close()).await;
    }
}

#[tokio::test]
async fn caps_join_connections_and_reuses_closed_slots() {
    let book = Book::new();
    book.set_join(true);
    let (a, mut events) = bind(book, Limits::default()).await;
    let mut clients = Vec::new();
    let mut joins = Vec::new();
    for _ in 0..5 {
        let book = Book::new();
        book.set(a.id(), Standing::Accepted);
        clients.push(raw_bind(book, Limits::default()).await);
    }
    let mut connections = Vec::new();
    for client in &clients[..4] {
        connections.push(client.connect(&a, JOIN_ALPN).await);
        match bounded(events.recv()).await.unwrap() {
            Event::Join { connection, .. } => joins.push(connection),
            _ => panic!("expected join"),
        }
    }
    closed(&clients[4].connect(&a, JOIN_ALPN).await, REFUSED).await;
    joins[0].close(0u32.into(), &[]);
    let connection = clients[4].connect(&a, JOIN_ALPN).await;
    assert!(matches!(
        bounded(events.recv()).await.unwrap(),
        Event::Join { .. }
    ));
    assert!(connection.close_reason().is_none());
    bounded(a.close()).await;
    for client in clients {
        bounded(client.close()).await;
    }
}

#[tokio::test]
async fn transport_allows_sixteen_bidirectional_and_no_unidirectional_streams() {
    let (a, b, _, _) = pair(Limits {
        frame: Duration::from_secs(10),
        ..Limits::default()
    })
    .await;
    let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
    let mut streams = Vec::new();
    for _ in 0..16 {
        streams.push(bounded(peer.connection().open_bi()).await.unwrap());
    }
    assert!(
        timeout(Duration::from_millis(500), peer.connection().open_bi())
            .await
            .is_err()
    );
    assert!(!matches!(
        timeout(Duration::from_millis(500), peer.connection().open_uni()).await,
        Ok(Ok(_))
    ));
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn idle_deadline_counts_only_time_without_open_streams() {
    let (a, b, _, _) = pair(Limits {
        idle: Duration::from_millis(400),
        frame: Duration::from_secs(10),
        ..Limits::default()
    })
    .await;
    let connection = raw(&b, &a, ALPN).await;
    let started = Instant::now();
    closed(&connection, IDLE).await;
    assert!((Duration::from_millis(300)..Duration::from_secs(3)).contains(&started.elapsed()));
    let connection = raw(&b, &a, ALPN).await;
    let (mut send, _recv) = bounded(connection.open_bi()).await.unwrap();
    bounded(send.write_all(&[0, 0])).await.unwrap();
    assert!(
        timeout(Duration::from_millis(1200), connection.closed())
            .await
            .is_err()
    );
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn stalled_frames_reset_and_leave_connection_usable() {
    let (a, b, _, mut events) = pair(Limits {
        frame: Duration::from_millis(300),
        ..Limits::default()
    })
    .await;
    let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
    let (mut send, mut recv) = bounded(peer.connection().open_bi()).await.unwrap();
    bounded(send.write_all(&[0, 0])).await.unwrap();
    match timeout(Duration::from_secs(2), recv.read_to_end(CONTROL_CAP))
        .await
        .unwrap()
    {
        Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code))) => {
            assert_eq!(code, BAD_FRAME.into())
        }
        other => panic!("expected a bad-frame reset, got {other:?}"),
    }
    empty(&mut events).await;
    bounded(peer.hello(&hello())).await.unwrap();
    assert!(matches!(
        bounded(events.recv()).await.unwrap(),
        Event::Hello { .. }
    ));
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn overcap_prefix_and_multiple_frames_reset_without_events() {
    let (a, b, _, mut events) = pair(Limits {
        frame: Duration::from_secs(10),
        ..Limits::default()
    })
    .await;
    let connection = raw(&b, &a, ALPN).await;
    let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
    bounded(send.write_all(&((CONTROL_CAP + 1) as u32).to_be_bytes()))
        .await
        .unwrap();
    match timeout(Duration::from_secs(2), recv.read_to_end(CONTROL_CAP))
        .await
        .unwrap()
    {
        Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code))) => {
            assert_eq!(code, BAD_FRAME.into())
        }
        other => panic!("expected a bad-frame reset, got {other:?}"),
    }
    empty(&mut events).await;
    let mut doubled = bytes(&Request::Hello { hello: hello() });
    doubled.extend(bytes(&Request::Hello { hello: hello() }));
    reset(&connection, &doubled).await;
    empty(&mut events).await;
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn another_fleet_closes_the_connection() {
    let (a, b, _, mut events) = pair(Limits::default()).await;
    let connection = raw(&b, &a, ALPN).await;
    let mut message = hello();
    message.fleet = "c".repeat(64);
    assert!(
        request(&connection, &Request::Hello { hello: message })
            .await
            .is_err()
    );
    closed(&connection, REFUSED).await;
    empty(&mut events).await;
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn every_stream_rechecks_admission_and_close_key_closes_all_connections() {
    let (a, b, book, mut events) = pair(Limits::default()).await;
    let connection = raw(&b, &a, ALPN).await;
    request(&connection, &Request::Hello { hello: hello() })
        .await
        .unwrap();
    assert!(matches!(
        bounded(events.recv()).await.unwrap(),
        Event::Hello { .. }
    ));
    book.set(b.id(), Standing::Refused);
    let _ = request(&connection, &Request::Hello { hello: hello() }).await;
    closed(&connection, REFUSED).await;
    empty(&mut events).await;
    book.set(b.id(), Standing::Accepted);
    let first = raw(&b, &a, ALPN).await;
    let second = raw(&b, &a, ALPN).await;
    for connection in [&first, &second] {
        request(connection, &Request::Hello { hello: hello() })
            .await
            .unwrap();
        bounded(events.recv()).await.unwrap();
    }
    a.close_key(&b.id());
    closed(&first, REFUSED).await;
    closed(&second, REFUSED).await;
    empty(&mut events).await;
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn closed_key_cannot_deliver_events_after_the_channel_drains() {
    for app in [true, false] {
        let book = Book::new();
        let a = bounded(radio_link::link::Link::bind(
            SecretKey::generate(),
            &["127.0.0.1:0".parse().unwrap()],
            book.clone(),
            Limits::default(),
            radio_link::link::Local {
                name: "garden".into(),
                sha: SHA.into(),
            },
        ))
        .await
        .unwrap();
        let (sender, mut events) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Event::Hello {
                from: a.id(),
                hello: hello(),
            })
            .unwrap();
        a.serve(sender);
        let b_book = Book::new();
        let (b, _) = bind(b_book.clone(), Limits::default()).await;
        book.set(b.id(), Standing::Accepted);
        b_book.set(a.id(), Standing::Accepted);
        let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
        let (mut send, mut recv) = bounded(peer.connection().open_bi()).await.unwrap();
        let request = if app {
            Request::App {
                to: a.id().to_string(),
                app: "lantern".into(),
                body: "opaque".into(),
            }
        } else {
            Request::Hello { hello: hello() }
        };
        bounded(frame::write(&mut send, &request, CONTROL_CAP))
            .await
            .unwrap();
        send.finish().unwrap();
        assert!(
            timeout(Duration::from_millis(100), recv.read_to_end(CONTROL_CAP))
                .await
                .is_err()
        );
        book.set(b.id(), Standing::Refused);
        a.close_key(&b.id());
        closed(peer.connection(), REFUSED).await;
        match bounded(events.recv()).await.unwrap() {
            Event::Hello { from, .. } => assert_eq!(from, a.id()),
            _ => panic!("expected filler event"),
        }
        assert!(
            timeout(Duration::from_millis(500), events.recv())
                .await
                .is_err(),
            "a closed key delivered an event"
        );
        bounded(a.close()).await;
        bounded(b.close()).await;
    }
}

#[tokio::test]
async fn refused_key_cannot_deliver_events_after_the_channel_drains() {
    for app in [true, false] {
        let book = Book::new();
        let a = bounded(radio_link::link::Link::bind(
            SecretKey::generate(),
            &["127.0.0.1:0".parse().unwrap()],
            book.clone(),
            Limits::default(),
            radio_link::link::Local {
                name: "garden".into(),
                sha: SHA.into(),
            },
        ))
        .await
        .unwrap();
        let (sender, mut events) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Event::Hello {
                from: a.id(),
                hello: hello(),
            })
            .unwrap();
        a.serve(sender);
        let b_book = Book::new();
        let (b, _) = bind(b_book.clone(), Limits::default()).await;
        book.set(b.id(), Standing::Accepted);
        b_book.set(a.id(), Standing::Accepted);
        let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
        let (mut send, mut recv) = bounded(peer.connection().open_bi()).await.unwrap();
        let request = if app {
            Request::App {
                to: a.id().to_string(),
                app: "lantern".into(),
                body: "opaque".into(),
            }
        } else {
            Request::Hello { hello: hello() }
        };
        bounded(frame::write(&mut send, &request, CONTROL_CAP))
            .await
            .unwrap();
        send.finish().unwrap();
        assert!(
            timeout(Duration::from_millis(100), recv.read_to_end(CONTROL_CAP))
                .await
                .is_err()
        );
        book.set(b.id(), Standing::Refused);
        // Change standing only: closing the connection here would abort the blocked task.
        assert!(peer.connection().close_reason().is_none());
        match bounded(events.recv()).await.unwrap() {
            Event::Hello { from, .. } => assert_eq!(from, a.id()),
            _ => panic!("expected filler event"),
        }
        timeout(Duration::from_secs(2), closed(peer.connection(), REFUSED))
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(500), events.recv())
                .await
                .is_err(),
            "a refused key delivered an event"
        );
        bounded(a.close()).await;
        bounded(b.close()).await;
    }
}

#[tokio::test]
async fn blocked_hello_events_bound_stream_tasks_and_recover_after_draining() {
    let book = Book::new();
    let a = bounded(radio_link::link::Link::bind(
        SecretKey::generate(),
        &["127.0.0.1:0".parse().unwrap()],
        book.clone(),
        Limits {
            frame: Duration::from_secs(10),
            ..Limits::default()
        },
        radio_link::link::Local {
            name: "garden".into(),
            sha: SHA.into(),
        },
    ))
    .await
    .unwrap();
    let (sender, mut events) = tokio::sync::mpsc::channel(1);
    sender
        .try_send(Event::Hello {
            from: a.id(),
            hello: hello(),
        })
        .unwrap();
    a.serve(sender);
    let b_book = Book::new();
    let (b, _) = bind(b_book.clone(), Limits::default()).await;
    book.set(b.id(), Standing::Accepted);
    b_book.set(a.id(), Standing::Accepted);
    let peer = bounded(b.dial(a.id(), &a.addrs())).await.unwrap();
    let mut streams = Vec::new();
    for _ in 0..16 {
        let (mut send, mut recv) = bounded(peer.connection().open_bi()).await.unwrap();
        bounded(frame::write(
            &mut send,
            &Request::Hello { hello: hello() },
            CONTROL_CAP,
        ))
        .await
        .unwrap();
        send.finish().unwrap();
        let _ = timeout(Duration::from_millis(30), recv.read_to_end(CONTROL_CAP)).await;
        streams.push((send, recv));
    }
    let extra = timeout(Duration::from_secs(1), async {
        let (mut send, mut recv) = peer.connection().open_bi().await.unwrap();
        let _ = frame::write(&mut send, &Request::Hello { hello: hello() }, CONTROL_CAP).await;
        let _ = send.finish();
        recv.read_to_end(CONTROL_CAP).await
    })
    .await;
    match extra {
        Err(_) => {}
        Ok(Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code)))) => {
            assert_eq!(code, BAD_FRAME.into());
        }
        other => panic!("expected a blocked stream or bad-frame reset, got {other:?}"),
    }
    drop(streams);
    for _ in 0..17 {
        assert!(matches!(
            bounded(events.recv()).await.unwrap(),
            Event::Hello { .. }
        ));
    }
    bounded(peer.hello(&hello())).await.unwrap();
    assert!(matches!(
        bounded(events.recv()).await.unwrap(),
        Event::Hello { .. }
    ));
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn join_deadline_closes_even_a_connection_held_by_the_receiver() {
    let book = Book::new();
    book.set_join(true);
    let (a, mut events) = bind(
        book,
        Limits {
            frame: Duration::from_millis(300),
            ..Limits::default()
        },
    )
    .await;
    let client_book = Book::new();
    client_book.set(a.id(), Standing::Accepted);
    let b = raw_bind(client_book, Limits::default()).await;
    let started = Instant::now();
    let connection = b.connect(&a, JOIN_ALPN).await;
    let accepted = match bounded(events.recv()).await.unwrap() {
        Event::Join { connection, .. } => connection,
        _ => panic!("expected join"),
    };
    timeout(Duration::from_secs(3), closed(&connection, IDLE))
        .await
        .unwrap();
    assert!((Duration::from_millis(200)..Duration::from_secs(3)).contains(&started.elapsed()));
    assert!(accepted.close_reason().is_some());
    bounded(a.close()).await;
    bounded(b.close()).await;
}

struct ClosingJoinBook(AtomicBool);

impl Admission for ClosingJoinBook {
    fn standing(&self, _: &radio_link::EndpointId) -> Standing {
        Standing::Unknown
    }
    fn join_open(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
    fn fleet(&self) -> Option<String> {
        Some(FLEET.into())
    }
}

#[tokio::test]
async fn join_rechecks_the_code_before_handing_out_the_connection() {
    let a = bounded(radio_link::link::Link::bind(
        SecretKey::generate(),
        &["127.0.0.1:0".parse().unwrap()],
        Arc::new(ClosingJoinBook(AtomicBool::new(true))),
        Limits::default(),
        radio_link::link::Local {
            name: "garden".into(),
            sha: SHA.into(),
        },
    ))
    .await
    .unwrap();
    let (sender, mut events) = tokio::sync::mpsc::channel(1);
    a.serve(sender);
    let book = Book::new();
    book.set(a.id(), Standing::Accepted);
    let b = raw_bind(book, Limits::default()).await;
    closed(&b.connect(&a, JOIN_ALPN).await, REFUSED).await;
    empty(&mut events).await;
    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn dials_without_addresses_fail_at_once() {
    let (a, b, book, _) = pair(Limits::default()).await;
    let start = Instant::now();
    assert_eq!(
        a.dial(b.id(), &[]).await.err(),
        Some(radio_link::endpoint::NO_ADDRESS)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    book.set(b.id(), Standing::Unknown);
    let start = Instant::now();
    assert_eq!(
        a.dial_join(b.id(), &[]).await.err(),
        Some(radio_link::endpoint::NO_ADDRESS)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    bounded(a.close()).await;
    bounded(b.close()).await;
}
