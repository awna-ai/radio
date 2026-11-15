mod common;

use common::{FLEET, SHA, bounded};
use radio_link::{
    EndpointId, SecretKey,
    admit::{Admission, Limits, Standing},
    handler::{Envelope, Handlers, Ran, Reply},
    link::{Link, Local},
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{Instant, sleep};

#[derive(Default)]
struct Book(Mutex<HashMap<EndpointId, Vec<SocketAddr>>>);

impl Book {
    fn accept(&self, other: &Link) {
        self.0.lock().unwrap().insert(other.id(), other.addrs());
    }
}

impl Admission for Book {
    fn standing(&self, key: &EndpointId) -> Standing {
        if self.0.lock().unwrap().contains_key(key) {
            Standing::Accepted
        } else {
            Standing::Unknown
        }
    }

    fn join_open(&self) -> bool {
        false
    }

    fn fleet(&self) -> Option<String> {
        Some(FLEET.into())
    }

    fn addrs(&self, key: &EndpointId) -> Vec<SocketAddr> {
        self.0.lock().unwrap().get(key).cloned().unwrap_or_default()
    }
}

struct Echo;

impl Handlers for Echo {
    fn has_handler(&self, app: &str) -> bool {
        app == "notes"
    }

    fn run(&self, envelope: &Envelope<'_>) -> Ran {
        Ran::Reply {
            reply: envelope.body.into(),
            exit: 0,
        }
    }

    fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, _reason: &str) {}
}

fn limits() -> Limits {
    Limits {
        unused: Duration::from_millis(400),
        // Each heartbeat must answer within one interval, on one thread shared by every link here,
        // so leave a loaded machine room or the kept connection fails and redials mid-test.
        heartbeat: Duration::from_millis(250),
        failed: Duration::from_secs(3),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
        idle: Duration::from_secs(2),
        ..Limits::default()
    }
}

async fn try_bind(
    key: SecretKey,
    addrs: &[SocketAddr],
    book: Arc<Book>,
    name: &str,
    limits: Limits,
) -> Result<Link, &'static str> {
    let link = bounded(Link::bind(
        key,
        addrs,
        book,
        limits,
        Local {
            name: name.into(),
            sha: SHA.into(),
            clients: None,
        },
        Arc::new(Echo),
    ))
    .await?;
    let (events, receiver) = tokio::sync::mpsc::channel(64);
    // These tests exchange app requests and heartbeats, neither of which emits events.
    drop(receiver);
    link.serve(events);
    Ok(link)
}

async fn bind(key: SecretKey, addrs: &[SocketAddr], book: Arc<Book>, name: &str) -> Link {
    try_bind(key, addrs, book, name, limits()).await.unwrap()
}

/// A closed link's port can stay taken for a moment, so retry until it is free.
async fn rebind(key: SecretKey, addrs: &[SocketAddr], book: Arc<Book>, name: &str) -> Link {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match try_bind(key.clone(), addrs, book.clone(), name, limits()).await {
            Ok(link) => return link,
            Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(50)).await,
            Err(error) => panic!("{error}"),
        }
    }
}

async fn computer(name: &str) -> (Link, Arc<Book>) {
    let book = Arc::new(Book::default());
    let link = bind(
        SecretKey::generate(),
        &["127.0.0.1:0".parse().unwrap()],
        book.clone(),
        name,
    )
    .await;
    (link, book)
}

async fn wait(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the connection state did not settle");
}

async fn message(sender: &Link, target: &Link, body: &str) -> iroh::endpoint::Connection {
    let peer = bounded(sender.peer(target.id(), &target.addrs()))
        .await
        .unwrap();
    assert_eq!(
        bounded(peer.send_app("notes", body)).await,
        Ok(Reply {
            reply: body.into(),
            exit: 0,
        })
    );
    peer.connection().clone()
}

#[tokio::test]
async fn a_second_send_reuses_the_first_connection() {
    let (a, a_book) = computer("garden").await;
    let (b, b_book) = computer("birch").await;
    a_book.accept(&b);
    b_book.accept(&a);

    let first = message(&a, &b, "ready").await;
    assert_eq!(b.live_connections(), 1);
    let second = message(&a, &b, "finished").await;
    assert_eq!(first.stable_id(), second.stable_id());
    assert_eq!(b.live_connections(), 1);

    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn of_three_members_the_one_no_one_uses_holds_no_connection() {
    let (a, a_book) = computer("garden").await;
    let (b, b_book) = computer("birch").await;
    let (c, c_book) = computer("cedar").await;
    for (book, others) in [
        (&a_book, [&b, &c]),
        (&b_book, [&a, &c]),
        (&c_book, [&a, &b]),
    ] {
        for other in others {
            book.accept(other);
        }
    }

    let connection = message(&a, &b, "ready").await;
    assert!(connection.close_reason().is_none());
    assert_eq!(a.live_connections(), 1);
    assert_eq!(b.live_connections(), 1);
    assert_eq!(c.live_connections(), 0);

    bounded(a.close()).await;
    bounded(b.close()).await;
    bounded(c.close()).await;
}

#[tokio::test]
async fn an_unused_connection_closes_and_a_kept_one_does_not() {
    let (a, a_book) = computer("garden").await;
    let (b, b_book) = computer("birch").await;
    let (c, c_book) = computer("cedar").await;
    a_book.accept(&b);
    a_book.accept(&c);
    b_book.accept(&a);
    c_book.accept(&a);

    let unused = message(&a, &b, "ready").await;
    a.keep("notes", vec![c.id()]);
    wait(|| a.kept("notes")[0].state.as_str() == "connected").await;
    let kept = message(&a, &c, "ready").await;
    assert!(matches!(
        bounded(unused.closed()).await,
        iroh::endpoint::ConnectionError::LocallyClosed
    ));
    wait(|| b.live_connections() == 0).await;
    // Observe the same kept connection through more than two unused intervals.
    bounded(async {
        let until = Instant::now() + Duration::from_millis(900);
        while Instant::now() < until {
            assert!(kept.close_reason().is_none());
            assert_eq!(a.kept("notes")[0].state.as_str(), "connected");
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(c.live_connections(), 1);

    bounded(a.close()).await;
    bounded(b.close()).await;
    bounded(c.close()).await;
}

#[tokio::test]
async fn a_kept_peer_whose_radio_restarts_is_redialed() {
    let (a, a_book) = computer("garden").await;
    let b_book = Arc::new(Book::default());
    let key = SecretKey::generate();
    let b = bind(
        key.clone(),
        &["127.0.0.1:0".parse().unwrap()],
        b_book.clone(),
        "birch",
    )
    .await;
    a_book.accept(&b);
    b_book.accept(&a);
    let id = b.id();
    let addrs = b.addrs();
    a.keep("notes", vec![id]);
    wait(|| a.kept("notes")[0].state.as_str() == "connected").await;
    let first = message(&a, &b, "ready").await;

    bounded(b.close()).await;
    drop(b);
    wait(|| matches!(a.kept("notes")[0].state.as_str(), "failed" | "dialing")).await;
    let b = rebind(key, &addrs, b_book, "birch").await;
    assert_eq!(b.id(), id);
    assert_eq!(b.addrs(), addrs);
    wait(|| a.kept("notes")[0].state.as_str() == "connected").await;
    let second = message(&a, &b, "finished").await;
    assert_ne!(first.stable_id(), second.stable_id());
    assert_eq!(b.live_connections(), 1);

    bounded(a.close()).await;
    bounded(b.close()).await;
}

#[tokio::test]
async fn kept_peers_leave_room_for_members_to_reach_us() {
    let (a, a_book) = computer("garden").await;
    let (c, c_book) = computer("cedar").await;
    let b_book = Arc::new(Book::default());
    let b = try_bind(
        SecretKey::generate(),
        &["127.0.0.1:0".parse().unwrap()],
        b_book.clone(),
        "birch",
        Limits {
            connections: 1,
            dialed: 1,
            ..limits()
        },
    )
    .await
    .unwrap();
    for (book, link) in [(&a_book, &a), (&c_book, &c)] {
        book.accept(&b);
        b_book.accept(link);
    }
    b.keep("notes", vec![a.id()]);
    wait(|| b.kept("notes")[0].state.as_str() == "connected").await;
    // The kept connection is one this side dialed, so the one place for incoming members is free.
    message(&c, &b, "through").await;
    // A second kept computer would be a second dialed connection: this side refuses it.
    b.keep("notes", vec![a.id(), c.id()]);
    wait(|| b.kept("notes")[1].state.as_str() == "failed").await;
    assert_eq!(b.kept("notes")[0].state.as_str(), "connected");

    bounded(a.close()).await;
    bounded(b.close()).await;
    bounded(c.close()).await;
}
