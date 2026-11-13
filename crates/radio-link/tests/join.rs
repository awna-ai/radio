mod common;

use common::{Book, SHA, bounded, closed, raw_bind};
use radio_link::{
    EndpointId, SecretKey,
    admit::{Admission, Limits, Standing},
    code::{Hints, JoinCode},
    computer_record::{self, ComputerRecord},
    endpoint::{ALPN, JOIN_ALPN},
    fleet_db::{Computer as Row, FleetDb, State},
    frame::{self, CONTROL_CAP, RECORD_CAP},
    join,
    link::{BAD_FRAME, Event, Link, Local, REFUSED},
    membership::{self, JoinAnswer, RecordBody, SignedRecord},
    wire::{Hello, JoinRequest, Protocol, Request},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "radio-join-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Computer {
    link: Arc<Link>,
    key: SecretKey,
    db: Arc<Mutex<FleetDb>>,
    applied: Arc<AtomicU64>,
    driver: JoinHandle<()>,
    server: JoinHandle<()>,
    _dir: Temp,
}
impl Drop for Computer {
    fn drop(&mut self) {
        self.driver.abort();
        self.server.abort();
    }
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
async fn computer(name: &str, limits: Limits) -> Computer {
    computer_key(name, limits, SecretKey::generate()).await
}
async fn computer_key(name: &str, limits: Limits, key: SecretKey) -> Computer {
    let dir = Temp::new();
    let db = Arc::new(Mutex::new(FleetDb::open(&dir.0.join("fleet.db")).unwrap()));
    let link = Arc::new(
        bounded(Link::bind(
            key.clone(),
            &["127.0.0.1:0".parse().unwrap()],
            db.clone() as Arc<dyn Admission>,
            limits,
            Local {
                name: name.into(),
                sha: SHA.into(),
            },
        ))
        .await
        .unwrap(),
    );
    let (sender, mut events) = mpsc::channel(64);
    let server = link.serve(sender);
    let (receiver_link, receiver_db, receiver_key) = (link.clone(), db.clone(), key.clone());
    let applied = Arc::new(AtomicU64::new(0));
    let applied_events = applied.clone();
    let driver = tokio::spawn(async move {
        let mut joins = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Join { connection, .. }) => {
                        let (link, db, key) = (receiver_link.clone(), receiver_db.clone(), receiver_key.clone());
                        joins.spawn(async move {
                            if let Ok(Some(record)) = join::answer(&link, &key, &db, connection, now()).await {
                                join::introduce(&link, &db, &record).await;
                            }
                        });
                    }
                    Some(Event::Record { record, .. }) => {
                        let _ = join::apply(&receiver_link, &receiver_db, &record, now());
                        applied_events.fetch_add(1, Ordering::SeqCst);
                    }
                    Some(Event::Computer { record, .. }) => {
                        let _ = join::apply_computer(&receiver_link, &receiver_db, &record);
                        applied_events.fetch_add(1, Ordering::SeqCst);
                    }
                    Some(_) => {},
                    None => break,
                },
                _ = joins.join_next(), if !joins.is_empty() => {},
            }
        }
    });
    Computer {
        link,
        key,
        db,
        applied,
        driver,
        server,
        _dir: dir,
    }
}
fn found(computer: &Computer) {
    computer
        .db
        .lock()
        .unwrap()
        .found(&computer.key, "harbor", &computer._dir.0.join("link"))
        .unwrap();
}
fn code(inviter: &Computer) -> JoinCode {
    inviter
        .db
        .lock()
        .unwrap()
        .create_code(
            &inviter.key,
            None,
            None,
            Hints {
                direct: inviter.link.addrs(),
            },
            now(),
        )
        .unwrap()
        .0
}
fn row(computer: &Computer, key: EndpointId) -> Option<Row> {
    computer
        .db
        .lock()
        .unwrap()
        .computers()
        .unwrap()
        .into_iter()
        .find(|r| r.key == key.to_string())
}
async fn state(computer: &Computer, key: EndpointId, expected: State) {
    bounded(async {
        loop {
            if row(computer, key).is_some_and(|r| r.state == expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
}
async fn enter(inviter: &Computer, newcomer: &Computer) {
    bounded(join::join(
        &newcomer.link,
        &newcomer.key,
        &newcomer.db,
        &code(inviter),
        now(),
    ))
    .await
    .unwrap();
}
async fn trio() -> (Computer, Computer, Computer) {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    let c = computer("cedar", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    enter(&a, &c).await;
    state(&b, c.link.id(), State::Pending).await;
    (a, b, c)
}
fn accept(computer: &Computer, other: &Computer) {
    computer
        .db
        .lock()
        .unwrap()
        .accept(&other.link.id().to_string())
        .unwrap();
}
fn hello(computer: &Computer) -> Hello {
    Hello {
        protocol: Protocol { major: 1, minor: 1 },
        fleet: computer
            .db
            .lock()
            .unwrap()
            .fleet()
            .unwrap()
            .unwrap()
            .body
            .id,
        sha: SHA.into(),
        arch: "aarch64".into(),
        name: computer.link.name().into(),
    }
}
async fn stop(computers: &[&Computer]) {
    for computer in computers {
        bounded(computer.link.close()).await;
    }
}
fn join_request(key: &SecretKey, code: &JoinCode, name: &str) -> JoinRequest {
    JoinRequest {
        code: code.encode(),
        consent: membership::consent(
            key,
            &code.fleet,
            &code.inviter.to_string(),
            name,
            &["127.0.0.1:7000".parse().unwrap()],
        ),
        name: name.into(),
        addrs: vec!["127.0.0.1:7000".parse().unwrap()],
    }
}
async fn raw_request(
    connection: &iroh::endpoint::Connection,
    request: &JoinRequest,
) -> Result<JoinAnswer, &'static str> {
    let (mut send, mut recv) = bounded(connection.open_bi()).await.map_err(|_| "closed")?;
    bounded(frame::write(&mut send, request, CONTROL_CAP)).await?;
    send.finish().map_err(|_| "closed")?;
    bounded(frame::read(&mut recv, RECORD_CAP)).await
}
fn raw_book(inviter: &Computer) -> Arc<Book> {
    let book = Book::new();
    book.set(inviter.link.id(), Standing::Accepted);
    book
}

#[tokio::test]
async fn a_newcomer_joins_through_the_code_and_takes_the_inviters_view() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    assert_eq!(
        a.db.lock().unwrap().fleet().unwrap(),
        b.db.lock().unwrap().fleet().unwrap()
    );
    assert_eq!(row(&b, a.link.id()).unwrap().state, State::Accepted);
    let newcomer = row(&a, b.link.id()).unwrap();
    assert_eq!(newcomer.state, State::Accepted);
    assert_eq!(newcomer.invited_by, Some(a.link.id().to_string()));
    assert!(a.db.lock().unwrap().codes(now()).unwrap().is_empty());
    stop(&[&a, &b]).await;
}
#[tokio::test]
async fn a_join_whose_newcomer_closes_at_once_is_still_introduced() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    assert!(!row(&b, a.link.id()).unwrap().trusted);
    let invitation = code(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let connection = raw.connect(&a.link, JOIN_ALPN).await;
    raw_request(&connection, &join_request(&raw.key, &invitation, "cedar"))
        .await
        .unwrap();
    connection.close(0u32.into(), &[]);
    assert_eq!(row(&a, raw.id()).unwrap().state, State::Accepted);
    state(&b, raw.id(), State::Pending).await;
    bounded(raw.close()).await;
    stop(&[&a, &b]).await;
}
#[tokio::test]
async fn an_untrusted_introduction_stays_pending_until_accepted() {
    let (a, b, c) = trio().await;
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    closed(peer.connection(), REFUSED).await;
    accept(&b, &c);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    assert_eq!(bounded(peer.hello(&hello(&c))).await.unwrap().name, "birch");
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn a_trusted_inviter_s_introduction_is_accepted_at_once() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    let d = computer("dune", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    b.db.lock()
        .unwrap()
        .trust(&a.link.id().to_string())
        .unwrap();
    enter(&a, &d).await;
    state(&b, d.link.id(), State::Accepted).await;
    stop(&[&a, &b, &d]).await;
}
#[tokio::test]
async fn a_blocked_computer_is_cut_off_and_never_dialed() {
    let (a, b, c) = trio().await;
    accept(&b, &c);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    bounded(peer.hello(&hello(&c))).await.unwrap();
    join::block(&b.link, &b.db, &c.link.id().to_string()).unwrap();
    closed(peer.connection(), REFUSED).await;
    let next = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    closed(next.connection(), REFUSED).await;
    assert_eq!(
        b.link.dial(c.link.id(), &c.link.addrs()).await.err(),
        Some("the computer is not let in")
    );
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn a_removal_reaches_the_others_and_shuts_the_key_out() {
    let (a, b, c) = trio().await;
    accept(&b, &c);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    bounded(peer.hello(&hello(&c))).await.unwrap();
    bounded(join::remove(
        &a.link,
        &a.key,
        &a.db,
        &c.link.id().to_string(),
        now(),
    ))
    .await
    .unwrap();
    state(&b, c.link.id(), State::Removed).await;
    bounded(peer.connection().closed()).await;
    assert_eq!(
        c.link.dial(b.link.id(), &b.link.addrs()).await.err(),
        Some("the computer is not let in")
    );
    let d = computer("dune", Limits::default()).await;
    enter(&a, &d).await;
    state(&b, d.link.id(), State::Pending).await;
    assert_eq!(
        join::remove(&b.link, &b.key, &b.db, &d.link.id().to_string(), now()).await,
        Err("only its inviter or the founding member removes a computer")
    );
    stop(&[&a, &b, &c, &d]).await;
}
#[tokio::test]
async fn a_leave_reaches_the_others() {
    let (a, b, c) = trio().await;
    accept(&b, &c);
    let first = bounded(c.link.dial(a.link.id(), &a.link.addrs()))
        .await
        .unwrap();
    let second = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    bounded(first.hello(&hello(&c))).await.unwrap();
    bounded(second.hello(&hello(&c))).await.unwrap();
    bounded(join::leave(&c.link, &c.key, &c.db, now()))
        .await
        .unwrap();
    state(&a, c.link.id(), State::Left).await;
    state(&b, c.link.id(), State::Left).await;
    assert!(c.db.lock().unwrap().fleet().unwrap().is_none());
    bounded(first.connection().closed()).await;
    bounded(second.connection().closed()).await;
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn a_tombstoned_key_with_a_valid_code_is_refused_and_the_code_still_works() {
    let (a, b, c) = trio().await;
    bounded(join::remove(
        &a.link,
        &a.key,
        &a.db,
        &c.link.id().to_string(),
        now(),
    ))
    .await
    .unwrap();
    let invitation = code(&a);
    let fresh = computer_key("cedar", Limits::default(), c.key.clone()).await;
    assert_eq!(
        bounded(join::join(
            &fresh.link,
            &fresh.key,
            &fresh.db,
            &invitation,
            now()
        ))
        .await,
        Err("the join was refused")
    );
    let e = computer("elm", Limits::default()).await;
    bounded(join::join(&e.link, &e.key, &e.db, &invitation, now()))
        .await
        .unwrap();
    stop(&[&a, &b, &c, &fresh, &e]).await;
}
#[tokio::test]
async fn a_known_name_with_a_new_key_is_refused() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    let fresh = computer("birch", Limits::default()).await;
    let invitation = code(&a);
    assert_eq!(
        bounded(join::join(
            &fresh.link,
            &fresh.key,
            &fresh.db,
            &invitation,
            now()
        ))
        .await,
        Err("the join was refused")
    );
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    stop(&[&a, &b, &fresh]).await;
}
#[tokio::test]
async fn a_code_whose_addresses_answer_with_another_key_fails() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    let c = computer(
        "cedar",
        Limits {
            dial: Duration::from_secs(1),
            ..Limits::default()
        },
    )
    .await;
    found(&a);
    found(&b);
    let mut invitation = code(&a);
    let _ = code(&b);
    invitation.hints.direct = b.link.addrs();
    assert!(
        bounded(join::join(&c.link, &c.key, &c.db, &invitation, now()))
            .await
            .is_err()
    );
    assert!(c.db.lock().unwrap().fleet().unwrap().is_none());
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn an_unknown_key_needs_a_live_code_and_only_on_the_join_alpn() {
    let a = computer("garden", Limits::default()).await;
    found(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    closed(&raw.connect(&a.link, JOIN_ALPN).await, REFUSED).await;
    let _ = code(&a);
    closed(&raw.connect(&a.link, ALPN).await, REFUSED).await;
    bounded(raw.close()).await;
    stop(&[&a]).await;
}
#[tokio::test]
async fn a_second_stream_on_a_join_connection_is_never_served() {
    let a = computer("garden", Limits::default()).await;
    found(&a);
    let invitation = code(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let connection = raw.connect(&a.link, JOIN_ALPN).await;
    let request = join_request(&raw.key, &invitation, "birch");
    assert!(raw_request(&connection, &request).await.is_ok());
    // Keep another live code open so that this checks the single-stream rule.
    let request = join_request(&raw.key, &code(&a), "birch");
    assert!(raw_request(&connection, &request).await.is_err());
    bounded(connection.closed()).await;
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    bounded(raw.close()).await;
    stop(&[&a]).await;
}
#[tokio::test]
async fn a_join_frame_that_stalls_is_closed_and_the_code_stays_open() {
    let a = computer(
        "garden",
        Limits {
            frame: Duration::from_secs(1),
            ..Limits::default()
        },
    )
    .await;
    found(&a);
    let _ = code(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let connection = raw.connect(&a.link, JOIN_ALPN).await;
    let (_send, _recv) = bounded(connection.open_bi()).await.unwrap();
    timeout(Duration::from_secs(3), connection.closed())
        .await
        .unwrap();
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    bounded(raw.close()).await;
    stop(&[&a]).await;
}
#[tokio::test]
async fn a_bad_name_or_address_in_a_join_request_is_refused() {
    let a = computer("garden", Limits::default()).await;
    found(&a);
    let invitation = code(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let mut bad_address = join_request(&raw.key, &invitation, "birch");
    bad_address.addrs = vec!["0.0.0.0:7000".parse().unwrap()];
    for request in [join_request(&raw.key, &invitation, "Bad"), bad_address] {
        let connection = raw.connect(&a.link, JOIN_ALPN).await;
        assert!(raw_request(&connection, &request).await.is_err());
        closed(&connection, REFUSED).await;
        assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    }
    bounded(raw.close()).await;
    stop(&[&a]).await;
}
#[tokio::test]
async fn a_forged_record_is_reset_without_an_event() {
    let a = computer("garden", Limits::default()).await;
    found(&a);
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let invitation = code(&a);
    a.db.lock()
        .unwrap()
        .admit(
            &a.key,
            a.link.name(),
            &a.link.addrs(),
            &invitation,
            &raw.id(),
            "birch",
            &["127.0.0.1:7000".parse().unwrap()],
            &membership::consent(
                &raw.key,
                &invitation.fleet,
                &a.link.id().to_string(),
                "birch",
                &["127.0.0.1:7000".parse().unwrap()],
            ),
            now(),
        )
        .unwrap();
    let receiver = bounded(Link::bind(
        a.key.clone(),
        &["127.0.0.1:0".parse().unwrap()],
        a.db.clone(),
        Limits::default(),
        Local {
            name: "garden".into(),
            sha: SHA.into(),
        },
    ))
    .await
    .unwrap();
    let (sender, mut events) = mpsc::channel(64);
    let server = receiver.serve(sender);
    let before = a.db.lock().unwrap().computers().unwrap();
    let other = SecretKey::generate();
    let mut record = membership::sign(
        RecordBody::Removal {
            fleet: invitation.fleet,
            signer: other.public().to_string(),
            counter: 1,
            subject: a.link.id().to_string(),
        },
        &other,
    );
    record.signature = "00".repeat(64);
    let connection = raw.connect(&receiver, ALPN).await;
    let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
    bounded(frame::write(
        &mut send,
        &Request::Record { record },
        CONTROL_CAP,
    ))
    .await
    .unwrap();
    send.finish().unwrap();
    match bounded(recv.read_to_end(CONTROL_CAP)).await {
        Err(iroh::endpoint::ReadToEndError::Read(iroh::endpoint::ReadError::Reset(code))) => {
            assert_eq!(code, BAD_FRAME.into())
        }
        other => panic!("expected bad-frame reset, got {other:?}"),
    }
    assert_eq!(a.db.lock().unwrap().computers().unwrap(), before);
    common::empty(&mut events).await;
    bounded(receiver.close()).await;
    server.abort();
    bounded(raw.close()).await;
    stop(&[&a]).await;
}
#[tokio::test]
async fn a_replayed_introduction_changes_nothing() {
    let (a, b, c) = trio().await;
    let fleet = a.db.lock().unwrap().fleet().unwrap().unwrap().body.id;
    let replay: SignedRecord = membership::sign(
        RecordBody::Introduction {
            fleet: fleet.clone(),
            inviter: a.link.id().to_string(),
            counter: 2,
            newcomer: c.link.id().to_string(),
            name: "cedar".into(),
            addrs: c.link.addrs(),
            consent: membership::consent(
                &c.key,
                &fleet,
                &a.link.id().to_string(),
                "cedar",
                &c.link.addrs(),
            ),
        },
        &a.key,
    );
    join::block(&b.link, &b.db, &c.link.id().to_string()).unwrap();
    let before = b.db.lock().unwrap().computers().unwrap();
    assert_eq!(
        join::apply(&b.link, &b.db, &replay, now()),
        Err("the record was already seen")
    );
    let processed = b.applied.load(Ordering::SeqCst);
    assert_eq!(
        bounded(join::deliver(
            &a.link,
            &replay,
            &[(b.link.id(), b.link.addrs())]
        ))
        .await,
        1
    );
    bounded(async {
        while b.applied.load(Ordering::SeqCst) == processed {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert_eq!(row(&b, c.link.id()).unwrap().state, State::Blocked);
    assert_eq!(b.db.lock().unwrap().computers().unwrap(), before);
    stop(&[&a, &b, &c]).await;
}

#[tokio::test]
async fn close_all_closes_every_live_key_and_frees_the_connection_slots() {
    let book = Book::new();
    book.set_join(true);
    let (server, mut events) = common::bind(
        book,
        Limits {
            connections: 2,
            ..Limits::default()
        },
    )
    .await;
    let first = raw_bind(Book::new(), Limits::default()).await;
    let second = raw_bind(Book::new(), Limits::default()).await;
    let mut held = Vec::new();
    let a = first.connect(&server, JOIN_ALPN).await;
    match bounded(events.recv()).await {
        Some(Event::Join { connection, .. }) => held.push(connection),
        other => panic!("expected join, got {other:?}"),
    }
    let b = second.connect(&server, JOIN_ALPN).await;
    match bounded(events.recv()).await {
        Some(Event::Join { connection, .. }) => held.push(connection),
        other => panic!("expected join, got {other:?}"),
    }
    server.close_all();
    closed(&a, REFUSED).await;
    closed(&b, REFUSED).await;
    // Both new admissions must fit after clearing all live entries.
    let c = first.connect(&server, JOIN_ALPN).await;
    match bounded(events.recv()).await {
        Some(Event::Join { connection, .. }) => held.push(connection),
        other => panic!("expected join, got {other:?}"),
    }
    let d = second.connect(&server, JOIN_ALPN).await;
    match bounded(events.recv()).await {
        Some(Event::Join { connection, .. }) => held.push(connection),
        other => panic!("expected join, got {other:?}"),
    }
    assert!(c.close_reason().is_none());
    assert!(d.close_reason().is_none());
    bounded(server.close()).await;
    bounded(first.close()).await;
    bounded(second.close()).await;
}

#[tokio::test]
async fn a_removed_computer_learns_it_and_forgets_its_fleet() {
    let (a, b, c) = trio().await;
    bounded(join::remove(
        &a.link,
        &a.key,
        &a.db,
        &c.link.id().to_string(),
        now(),
    ))
    .await
    .unwrap();
    bounded(async {
        while c.db.lock().unwrap().fleet().unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    stop(&[&a, &b, &c]).await;
}

async fn connection_to_aborted_driver(b: &Computer, c: &Computer) -> radio_link::link::Peer {
    accept(b, c);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    bounded(peer.hello(&hello(c))).await.unwrap();
    b.driver.abort();
    bounded(async {
        while !b.driver.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(peer.connection().close_reason().is_none());
    peer
}

#[tokio::test]
async fn leaving_closes_every_connection_even_where_the_leave_was_not_applied() {
    let (a, b, c) = trio().await;
    let peer = connection_to_aborted_driver(&b, &c).await;
    bounded(join::leave(&c.link, &c.key, &c.db, now()))
        .await
        .unwrap();
    assert_eq!(
        bounded(peer.connection().closed()).await,
        iroh::endpoint::ConnectionError::LocallyClosed
    );
    assert_eq!(row(&b, c.link.id()).unwrap().state, State::Accepted);
    stop(&[&a, &b, &c]).await;
}

#[tokio::test]
async fn being_removed_closes_every_connection() {
    let (a, b, c) = trio().await;
    let peer = connection_to_aborted_driver(&b, &c).await;
    bounded(join::remove(
        &a.link,
        &a.key,
        &a.db,
        &c.link.id().to_string(),
        now(),
    ))
    .await
    .unwrap();
    assert_eq!(
        bounded(peer.connection().closed()).await,
        iroh::endpoint::ConnectionError::LocallyClosed
    );
    assert_eq!(row(&b, c.link.id()).unwrap().state, State::Accepted);
    stop(&[&a, &b, &c]).await;
}

#[tokio::test]
async fn joining_with_another_links_key_is_refused() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    let invitation = code(&a);
    assert_eq!(
        bounded(join::join(&b.link, &a.key, &b.db, &invitation, now())).await,
        Err("the key is not this link's")
    );
    assert!(b.db.lock().unwrap().fleet().unwrap().is_none());
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap().len(), 1);
    stop(&[&a, &b]).await;
}

fn resign(computer: &Computer, section: &str) -> ComputerRecord {
    computer
        .db
        .lock()
        .unwrap()
        .resign(
            &computer.key,
            computer.link.name(),
            &computer.link.addrs(),
            "lantern",
            Some(section),
            Some(&["hub".into()]),
            now(),
        )
        .unwrap()
}
async fn holds(computer: &Computer, record: &ComputerRecord) {
    bounded(async {
        loop {
            if computer
                .db
                .lock()
                .unwrap()
                .computer_records()
                .unwrap()
                .contains(record)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}
async fn accepted_trio() -> (Computer, Computer, Computer) {
    let (a, b, c) = trio().await;
    for (computer, other) in [(&a, &b), (&a, &c), (&b, &a), (&b, &c), (&c, &a), (&c, &b)] {
        accept(computer, other);
    }
    (a, b, c)
}
#[tokio::test]
async fn a_resigned_record_reaches_the_others() {
    let (a, b, c) = accepted_trio().await;
    let first = resign(&a, "{\"x\":1}");
    assert_eq!(bounded(join::publish(&a.link, &a.db, &first)).await, 2);
    holds(&b, &first).await;
    holds(&c, &first).await;
    let second = resign(&a, "{\"x\":2}");
    assert!(second.body.version > first.body.version);
    assert_eq!(bounded(join::publish(&a.link, &a.db, &second)).await, 2);
    holds(&b, &second).await;
    holds(&c, &second).await;
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn an_older_computer_record_changes_nothing() {
    let (a, b, c) = accepted_trio().await;
    let first = resign(&a, "first");
    assert_eq!(bounded(join::publish(&a.link, &a.db, &first)).await, 2);
    holds(&b, &first).await;
    holds(&c, &first).await;
    let second = resign(&a, "second");
    assert_eq!(bounded(join::publish(&a.link, &a.db, &second)).await, 2);
    holds(&b, &second).await;
    holds(&c, &second).await;
    bounded(async {
        while b.applied.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let before = b.applied.load(Ordering::SeqCst);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    assert_eq!(bounded(peer.send_computer(&first)).await, Ok(()));
    bounded(async {
        while b.applied.load(Ordering::SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        b.db.lock().unwrap().own_record(&a.link.id()).unwrap(),
        Some(second)
    );
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn a_forged_computer_record_is_never_served() {
    let (a, b, c) = accepted_trio().await;
    let first = resign(&a, "first");
    assert_eq!(bounded(join::publish(&a.link, &a.db, &first)).await, 2);
    holds(&b, &first).await;
    let mut body = first.body.clone();
    body.version += 1;
    body.apps.insert("lantern".into(), "forged".into());
    let forged = computer_record::sign(body, &c.key);
    bounded(async {
        while b.applied.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let before = b.applied.load(Ordering::SeqCst);
    let peer = bounded(c.link.dial(b.link.id(), &b.link.addrs()))
        .await
        .unwrap();
    assert!(bounded(peer.send_computer(&forged)).await.is_err());
    assert_eq!(b.applied.load(Ordering::SeqCst), before);
    assert_eq!(
        b.db.lock().unwrap().own_record(&a.link.id()).unwrap(),
        Some(first)
    );
    stop(&[&a, &b, &c]).await;
}
#[tokio::test]
async fn a_newcomer_takes_the_records_its_inviter_holds() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    let record = resign(&a, "opaque");
    enter(&a, &b).await;
    assert_eq!(
        b.db.lock().unwrap().own_record(&a.link.id()).unwrap(),
        Some(record)
    );
    stop(&[&a, &b]).await;
}
#[tokio::test]
async fn a_computer_record_from_a_stranger_is_refused() {
    let a = computer("garden", Limits::default()).await;
    let b = computer("birch", Limits::default()).await;
    found(&a);
    enter(&a, &b).await;
    let record = resign(&b, "opaque");
    let raw = raw_bind(raw_book(&a), Limits::default()).await;
    let before = a.applied.load(Ordering::SeqCst);
    let connection = raw.connect(&a.link, ALPN).await;
    if let Ok((mut send, _)) = bounded(connection.open_bi()).await {
        let _ = bounded(frame::write(
            &mut send,
            &Request::Computer { record },
            CONTROL_CAP,
        ))
        .await;
        let _ = send.finish();
    }
    closed(&connection, REFUSED).await;
    assert_eq!(a.applied.load(Ordering::SeqCst), before);
    bounded(raw.close()).await;
    stop(&[&a, &b]).await;
}
