mod common;

use common::{SHA, bounded};
use radio_link::{
    SecretKey,
    admit::Limits,
    code::{Hints, JoinCode},
    fleet_db::{FleetDb, State},
    link::{Local, REFUSED},
    node::Node,
};
use std::{
    fs,
    net::UdpSocket,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Computer {
    node: Node,
    key: SecretKey,
    db: Arc<Mutex<FleetDb>>,
    dir: Temp,
}

impl Computer {
    fn new(name: &str) -> Self {
        Self::with_handlers(name, Arc::new(radio_link::handler::NoHandlers))
    }

    fn with_handlers(name: &str, handlers: Arc<dyn radio_link::handler::Handlers>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "radio-node-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let db = Arc::new(Mutex::new(FleetDb::open(&dir.join("fleet.db")).unwrap()));
        let key = SecretKey::generate();
        let node = Node::start(
            key.clone(),
            &["127.0.0.1:0".parse().unwrap()],
            db.clone(),
            Local {
                name: name.into(),
                sha: SHA.into(),
                clients: None,
            },
            Limits::default(),
            handlers,
        )
        .unwrap();
        Self {
            node,
            key,
            db,
            dir: Temp(dir),
        }
    }

    /// Stops this computer's Radio and starts it again on the same key, address and store.
    fn restart(self, name: &str, handlers: Arc<dyn radio_link::handler::Handlers>) -> Self {
        let Self { node, key, db, dir } = self;
        let addrs = node.addrs();
        drop(node);
        let node = Node::start(
            key.clone(),
            &addrs,
            db.clone(),
            Local {
                name: name.into(),
                sha: SHA.into(),
                clients: None,
            },
            Limits::default(),
            handlers,
        )
        .unwrap();
        Self { node, key, db, dir }
    }

    fn status(&self) -> serde_json::Value {
        let path = self.dir.0.join("status.json");
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn found(&self) {
        self.db
            .lock()
            .unwrap()
            .found(&self.key, "harbor", &self.dir.0.join("link"))
            .unwrap();
    }

    fn code(&self) -> JoinCode {
        self.db
            .lock()
            .unwrap()
            .create_code(
                &self.key,
                None,
                None,
                Hints {
                    direct: self.node.addrs(),
                },
                now(),
            )
            .unwrap()
            .0
    }

    fn state(&self, other: &Self) -> Option<State> {
        self.db
            .lock()
            .unwrap()
            .computers()
            .unwrap()
            .into_iter()
            .find(|row| row.key == other.node.id().to_string())
            .map(|row| row.state)
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn wait(mut ready: impl FnMut() -> bool) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(bounded(async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }));
}

fn joined() -> (Computer, Computer) {
    let a = Computer::new("garden");
    let b = Computer::new("birch");
    a.found();
    assert_eq!(b.node.join(a.code(), now()), Ok(()));
    wait(|| a.state(&b) == Some(State::Accepted));
    wait(|| {
        a.node.link().topic_neighbors().contains(&b.node.id())
            && b.node.link().topic_neighbors().contains(&a.node.id())
    });
    (a, b)
}

#[test]
fn a_daemon_joins_through_a_code() {
    let (a, b) = joined();
    assert_eq!(a.state(&b), Some(State::Accepted));
    assert_eq!(
        b.db.lock().unwrap().fleet().unwrap(),
        a.db.lock().unwrap().fleet().unwrap()
    );
}

#[test]
fn a_configured_address_with_another_key_is_refused() {
    let a = Computer::new("garden");
    let b = Computer::new("birch");
    let c = Computer::new("cedar");
    a.found();
    let mut code = a.code();
    code.hints.direct = c.node.addrs();
    assert_eq!(
        b.node.join(code, now()),
        Err("the computer could not be reached")
    );
    assert!(b.db.lock().unwrap().fleet().unwrap().is_none());
    assert_eq!(a.db.lock().unwrap().codes(now()).unwrap()[0].uses, 1);
    assert!(c.state(&b).is_none());
}

#[test]
fn a_code_without_hints_fails_fast() {
    let a = Computer::new("garden");
    let b = Computer::new("birch");
    a.found();
    let mut code = a.code();
    code.hints.direct.clear();
    let start = Instant::now();
    assert_eq!(
        b.node.join(code, now()),
        Err("the join code has no address to dial")
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn remove_reaches_its_subject() {
    let (a, b) = joined();
    assert!(a.node.remove(&b.node.id().to_string(), now()).unwrap() >= 1);
    wait(|| b.db.lock().unwrap().fleet().unwrap().is_none());
    assert_eq!(a.state(&b), Some(State::Removed));
}

#[test]
fn leave_reaches_the_others() {
    let (a, b) = joined();
    assert_eq!(b.node.leave(now()), Ok(1));
    wait(|| a.state(&b) == Some(State::Left));
    assert!(b.db.lock().unwrap().fleet().unwrap().is_none());
}

#[test]
fn block_closes_the_keys_connections() {
    let (a, b) = joined();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let peer = runtime
        .block_on(bounded(b.node.link().dial(a.node.id(), &a.node.addrs())))
        .unwrap();
    assert_eq!(
        runtime.block_on(bounded(peer.send_app("lantern", "ready"))),
        Err("that app takes no messages on that computer")
    );
    a.node.block(&b.node.id().to_string()).unwrap();
    runtime.block_on(common::closed(peer.connection(), REFUSED));
}

#[test]
fn blocking_a_computer_closes_its_topic_connection() {
    let (a, b) = joined();
    assert!(b.node.link().topic_accepted().contains(&a.node.id()));
    b.node.block(&a.node.id().to_string()).unwrap();
    assert!(!b.node.link().topic_accepted().contains(&a.node.id()));
    wait(|| {
        !a.node.link().topic_neighbors().contains(&b.node.id())
            && !b.node.link().topic_neighbors().contains(&a.node.id())
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _ = runtime.block_on(bounded(a.node.link().sync_topic()));
    std::thread::sleep(Duration::from_secs(3));
    assert!(!b.node.link().topic_neighbors().contains(&a.node.id()));
}

#[test]
fn publish_is_applied_by_the_others() {
    let (a, b) = joined();
    let record =
        b.db.lock()
            .unwrap()
            .resign(
                &b.key,
                "birch",
                &b.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    let start = Instant::now();
    b.node.publish(record.clone());
    assert!(start.elapsed() < Duration::from_secs(1));
    wait(|| {
        a.db.lock()
            .unwrap()
            .announcement(&b.node.id())
            .unwrap()
            .is_some()
    });
    assert_ne!(
        a.db.lock().unwrap().computer_record(&b.node.id()).unwrap(),
        Some(record.clone())
    );
    a.node.fetch_records();
    wait(|| {
        a.db.lock()
            .unwrap()
            .computer_records()
            .unwrap()
            .contains(&record)
    });
}

#[test]
fn drop_stops_the_link() {
    let a = Computer::new("garden");
    let addr = a.node.addrs()[0];
    drop(a);
    let socket = UdpSocket::bind(addr).unwrap();
    drop(socket);
}

#[test]
fn operations_finish_after_the_endpoint_closes() {
    let a = Computer::new("garden");
    a.found();
    let mut code = a.code();
    code.hints.direct.clear();
    let b = Computer::new("birch");
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(bounded(b.node.link().close()));
        wait(|| !b.node.running());
        for _ in 0..10 {
            let results = (
                b.node.join(code.clone(), now()),
                b.node.remove("cedar", now()),
                b.node.leave(now()),
            );
            sender.send(results).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    for _ in 0..10 {
        let (join, remove, leave) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(join, Err("the link stopped"));
        assert!(remove.is_err());
        assert!(leave.is_err());
    }
    worker.join().unwrap();
}

#[test]
fn a_closed_endpoint_reads_as_stopped() {
    let a = Computer::new("garden");
    a.found();
    let code = a.code();
    let b = Computer::new("birch");
    assert!(b.node.running());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(bounded(b.node.link().close()));
    wait(|| !b.node.running());
    let start = Instant::now();
    assert_eq!(b.node.join(code, now()), Err("the link stopped"));
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn an_introduction_reaches_the_other_computers() {
    let (a, b) = joined();
    let c = Computer::new("cedar");
    assert_eq!(c.node.join(a.code(), now()), Ok(()));
    wait(|| b.state(&c).is_some());
    assert_eq!(b.state(&c), Some(State::Pending));
}

#[test]
fn quick_publishes_share_one_announcer() {
    let (a, b) = joined();
    let resign = |section| {
        b.db.lock()
            .unwrap()
            .resign(
                &b.key,
                "birch",
                &b.node.addrs(),
                "lantern",
                Some(section),
                None,
                now(),
            )
            .unwrap()
    };
    let first = resign("ready");
    b.node.publish(first.clone());
    wait(|| {
        a.db.lock()
            .unwrap()
            .announcement(&b.node.id())
            .unwrap()
            .is_some()
    });
    let held =
        a.db.lock()
            .unwrap()
            .announcement(&b.node.id())
            .unwrap()
            .unwrap();
    assert!(held.matches(&first));
    let latest = resign("finished");
    b.node.publish(latest);
    wait(|| b.node.published() == 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        a.db.lock().unwrap().announcement(&b.node.id()).unwrap(),
        Some(held)
    );
    assert_eq!(b.node.published(), 2);
}

fn gossip_trio() -> (Computer, Computer, Computer) {
    let (a, b) = joined();
    let c = Computer::new("cedar");
    c.node.join(b.code(), now()).unwrap();
    wait(|| a.state(&c).is_some());
    // C stays pending at A, so the gate forbids A–C connections in either direction.
    assert_eq!(a.state(&c), Some(State::Pending));
    for (computer, other) in [(&a, &c), (&c, &a)] {
        // The outer computers bootstrap only through the middle computer.
        let db = rusqlite::Connection::open(computer.dir.0.join("fleet.db")).unwrap();
        db.execute(
            "UPDATE computers SET addrs = '[]', last_addr = NULL WHERE key = ?1",
            [other.node.id().to_string()],
        )
        .unwrap();
    }
    let a = a.restart("garden", Arc::new(radio_link::handler::NoHandlers));
    let c = c.restart("cedar", Arc::new(radio_link::handler::NoHandlers));
    assert!(
        a.db.lock()
            .unwrap()
            .computer(&c.node.id().to_string())
            .unwrap()
            .addrs
            .is_empty()
    );
    assert!(
        c.db.lock()
            .unwrap()
            .computer(&a.node.id().to_string())
            .unwrap()
            .addrs
            .is_empty()
    );
    wait(|| {
        a.node.link().topic_neighbors().contains(&b.node.id())
            && c.node.link().topic_neighbors().contains(&b.node.id())
    });
    assert!(!a.node.link().connected_to(&c.node.id()));
    assert!(!c.node.link().connected_to(&a.node.id()));
    (a, b, c)
}

fn broadcast(computer: &Computer, message: radio_link::topic::TopicMessage) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(bounded(computer.node.link().broadcast(&message)))
        .unwrap();
}

#[test]
fn a_removal_crosses_the_fleet_topic_without_a_stored_outer_address() {
    let (a, b, c) = gossip_trio();
    let d = Computer::new("aspen");
    d.node.join(a.code(), now()).unwrap();
    wait(|| b.state(&d).is_some() && c.state(&d).is_some());
    a.node.remove(&d.node.id().to_string(), now()).unwrap();
    wait(|| c.state(&d) == Some(State::Removed));
    assert!(!a.node.link().connected_to(&c.node.id()));
    assert!(!c.node.link().connected_to(&a.node.id()));
}

#[test]
fn an_announcement_crosses_the_topic_and_is_fetched_only_on_demand() {
    let (a, b, c) = gossip_trio();
    // First prove that the middle computer can forward to the last one.
    let d = Computer::new("aspen");
    d.node.join(a.code(), now()).unwrap();
    wait(|| c.state(&d).is_some());
    let record =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    a.node.publish(record.clone());
    wait(|| {
        c.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .is_some()
    });
    let held =
        c.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .unwrap();
    assert!(held.matches(&record));
    assert_ne!(
        c.db.lock().unwrap().computer_record(&a.node.id()).unwrap(),
        Some(record.clone())
    );
    b.node.fetch_records();
    wait(|| b.db.lock().unwrap().computer_record(&a.node.id()).unwrap() == Some(record.clone()));
    c.node.fetch_records();
    wait(|| c.db.lock().unwrap().computer_record(&a.node.id()).unwrap() == Some(record.clone()));
    assert!(!a.node.link().connected_to(&c.node.id()));
    assert!(!c.node.link().connected_to(&a.node.id()));
}

#[test]
fn a_forged_topic_announcement_does_not_change_the_held_one() {
    let (a, _b, c) = gossip_trio();
    let d = Computer::new("aspen");
    d.node.join(a.code(), now()).unwrap();
    wait(|| c.state(&d).is_some());
    let record =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    let before = c.db.lock().unwrap().announcement(&a.node.id()).unwrap();
    assert!(before.is_none());
    let forged = radio_link::topic::announce(&record, &SecretKey::generate());
    broadcast(&a, radio_link::topic::TopicMessage::Announcement(forged));
    // The removal is a barrier through the same topic; no receive window masks the forgery.
    a.node.remove(&d.node.id().to_string(), now()).unwrap();
    wait(|| c.state(&d) == Some(State::Removed));
    assert_eq!(
        c.db.lock().unwrap().announcement(&a.node.id()).unwrap(),
        before
    );
    a.node.publish(record.clone());
    wait(|| {
        c.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .is_some()
    });
    assert!(
        c.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .unwrap()
            .matches(&record)
    );
}

#[test]
fn an_on_demand_fetch_drops_a_record_that_does_not_match_its_announcement() {
    let (a, b) = joined();
    let actual =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("stored"),
                None,
                now(),
            )
            .unwrap();
    let mut body = actual.body.clone();
    body.apps.insert("lantern".into(), "announced".into());
    let announced = radio_link::computer_record::sign(body, &a.key);
    let before = b.db.lock().unwrap().computer_record(&a.node.id()).unwrap();
    a.node.publish(announced.clone());
    wait(|| {
        b.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .is_some()
    });
    let (_, finished) = b.node.link().fetch_counts();
    b.node.fetch_records();
    wait(|| b.node.link().fetch_counts().1 > finished);
    assert_eq!(
        b.db.lock().unwrap().computer_record(&a.node.id()).unwrap(),
        before
    );
    assert!(
        b.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .unwrap()
            .matches(&announced)
    );
}

#[test]
fn dialing_a_member_fetches_its_newly_announced_record() {
    let (a, b) = joined();
    let record =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    a.node.publish(record.clone());
    wait(|| {
        b.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .is_some()
    });
    // Use the daemon's runtime through a normal message, which calls Link::peer.
    let _ = b.node.message(&a.node.id().to_string(), "lantern", "ready");
    wait(|| b.db.lock().unwrap().computer_record(&a.node.id()).unwrap() == Some(record.clone()));
}

#[test]
fn an_outsider_cannot_connect_to_the_fleet_topic() {
    let (a, _b) = joined();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(bounded(async {
        let book = common::Book::new();
        book.set(a.node.id(), radio_link::admit::Standing::Accepted);
        let outsider = common::raw_bind(book, Limits::default()).await;
        let connection = outsider
            .connect(a.node.link(), radio_link::endpoint::GOSSIP_ALPN)
            .await;
        common::closed(&connection, REFUSED).await;
        outsider.close().await;
    }));
}

#[test]
fn a_message_reaches_the_recorded_name_and_returns_a_reply() {
    struct Echo;
    impl radio_link::handler::Handlers for Echo {
        fn has_handler(&self, app: &str) -> bool {
            app == "notes"
        }
        fn run(&self, envelope: &radio_link::handler::Envelope<'_>) -> radio_link::handler::Ran {
            radio_link::handler::Ran::Reply {
                reply: format!(
                    "{}:{}",
                    envelope.from_name.unwrap_or("missing"),
                    envelope.body
                ),
                exit: 3,
            }
        }
        fn refused(
            &self,
            _from: &radio_link::EndpointId,
            _name: Option<&str>,
            _app: &str,
            _reason: &str,
        ) {
        }
    }
    let target = Computer::with_handlers("garden", Arc::new(Echo));
    let sender = Computer::new("birch");
    target.found();
    assert_eq!(sender.node.join(target.code(), now()), Ok(()));
    wait(|| target.state(&sender) == Some(State::Accepted));
    assert_eq!(
        sender.node.message("garden", "notes", "opaque"),
        Ok(radio_link::handler::Reply {
            reply: "birch:opaque".into(),
            exit: 3
        })
    );
}

struct Notes;
impl radio_link::handler::Handlers for Notes {
    fn has_handler(&self, app: &str) -> bool {
        app == "notes"
    }
    fn run(&self, envelope: &radio_link::handler::Envelope<'_>) -> radio_link::handler::Ran {
        radio_link::handler::Ran::Reply {
            reply: envelope.body.to_owned(),
            exit: 0,
        }
    }
    fn refused(&self, _: &radio_link::EndpointId, _: Option<&str>, _: &str, _: &str) {}
}

fn listed(status: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    status["computers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == key)
        .cloned()
}

#[test]
fn restarted_radios_reconnect_from_the_stored_address_without_a_code() {
    let target = Computer::with_handlers("garden", Arc::new(Notes));
    let sender = Computer::new("birch");
    target.found();
    assert_eq!(sender.node.join(target.code(), now()), Ok(()));
    wait(|| target.state(&sender) == Some(State::Accepted));
    let target = target.restart("garden", Arc::new(Notes));
    let sender = sender.restart("birch", Arc::new(radio_link::handler::NoHandlers));
    for side in [&target, &sender] {
        assert_eq!(side.status()["computers"], serde_json::json!([]));
    }
    assert_eq!(
        sender.node.message("garden", "notes", "again"),
        Ok(radio_link::handler::Reply {
            reply: "again".into(),
            exit: 0
        })
    );
    let key = target.node.id().to_string();
    assert_eq!(
        sender.db.lock().unwrap().last_addr(&key).unwrap(),
        Some(target.node.addrs()[0])
    );
}

#[test]
fn status_lists_a_connected_computer_until_its_connection_closes() {
    let (a, b) = joined();
    let a = a.restart("garden", Arc::new(Notes));
    let b_key = b.node.id().to_string();
    let a_key = a.node.id().to_string();
    assert_eq!(
        b.node
            .message("garden", "notes", "ready")
            .map(|reply| reply.reply),
        Ok("ready".into())
    );
    wait(|| listed(&b.status(), &a_key).is_some());
    let entry = listed(&b.status(), &a_key).unwrap();
    assert_eq!(entry["name"], "garden");
    assert_eq!(entry["path"], "direct");
    assert_eq!(entry["addr"], a.node.addrs()[0].to_string());
    assert!(entry["rtt_ms"].is_u64());
    assert!(entry["last_seen"].as_u64().unwrap() >= now() as u64 - 60);
    assert_eq!(b.status()["relay"], "none");
    wait(|| listed(&a.status(), &b_key).is_some());
    assert_eq!(listed(&a.status(), &b_key).unwrap()["name"], "birch");
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(b.dir.0.join("status.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    a.node.block(&b_key).unwrap();
    wait(|| listed(&a.status(), &b_key).is_none());
    wait(|| listed(&b.status(), &a_key).is_none());
}

#[test]
fn the_topic_is_joined_again_after_its_connections_close() {
    let (a, b) = joined();
    a.node.link().close_connections();
    b.node.link().close_connections();
    wait(|| {
        a.node.link().topic_neighbors().is_empty() && b.node.link().topic_neighbors().is_empty()
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !a.node.link().topic_neighbors().contains(&b.node.id())
        || !b.node.link().topic_neighbors().contains(&a.node.id())
    {
        assert!(
            Instant::now() < deadline,
            "the topic neighbours did not return"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let record =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    a.node.publish(record.clone());
    wait(|| {
        b.db.lock()
            .unwrap()
            .announcement(&a.node.id())
            .unwrap()
            .is_some_and(|held| held.matches(&record))
    });
}

#[test]
fn fetching_records_never_waits_for_an_offline_computer() {
    let (a, b) = joined();
    let id = b.node.id();
    let before =
        b.db.lock()
            .unwrap()
            .resign(
                &b.key,
                "birch",
                &b.node.addrs(),
                "lantern",
                Some("stored"),
                None,
                now(),
            )
            .unwrap();
    a.db.lock()
        .unwrap()
        .apply_computer(&a.node.id(), &before)
        .unwrap();
    let record =
        b.db.lock()
            .unwrap()
            .resign(
                &b.key,
                "birch",
                &b.node.addrs(),
                "lantern",
                Some("ready"),
                None,
                now(),
            )
            .unwrap();
    a.db.lock()
        .unwrap()
        .apply_announcement(&a.node.id(), &radio_link::topic::announce(&record, &b.key))
        .unwrap();
    drop(b);
    let start = Instant::now();
    a.node.fetch_records();
    let held = a.db.lock().unwrap().computer_record(&id).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "fetching records waited for the network"
    );
    assert_eq!(held, Some(before));
}

#[test]
fn leaving_without_a_topic_neighbour_does_not_drain() {
    let a = Computer::new("garden");
    a.found();
    assert!(a.node.link().topic_neighbors().is_empty());
    let start = Instant::now();
    assert_eq!(a.node.leave(now()), Ok(0));
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn repeated_peer_calls_share_a_fetch_and_its_failure_back_off() {
    let (a, b) = joined();
    let stored =
        a.db.lock()
            .unwrap()
            .resign(
                &a.key,
                "garden",
                &a.node.addrs(),
                "lantern",
                Some("stored"),
                None,
                now(),
            )
            .unwrap();
    let mut body = stored.body.clone();
    body.apps.insert("lantern".into(), "announced".into());
    let announced = radio_link::computer_record::sign(body, &a.key);
    b.db.lock()
        .unwrap()
        .apply_announcement(
            &b.node.id(),
            &radio_link::topic::announce(&announced, &a.key),
        )
        .unwrap();
    assert!(
        b.db.lock()
            .unwrap()
            .fetch_wanted(&a.node.id())
            .unwrap()
            .is_some()
    );
    let (started, finished) = b.node.link().fetch_counts();
    for _ in 0..2 {
        let _ = b.node.message(&a.node.id().to_string(), "lantern", "ready");
    }
    assert_eq!(b.node.link().fetch_counts().0, started + 1);
    wait(|| b.node.link().fetch_counts().1 == finished + 1);
    assert_ne!(
        b.db.lock().unwrap().computer_record(&a.node.id()).unwrap(),
        Some(announced)
    );
    let _ = b.node.message(&a.node.id().to_string(), "lantern", "ready");
    assert_eq!(b.node.link().fetch_counts().0, started + 1);
}
