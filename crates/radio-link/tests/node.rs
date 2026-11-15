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
fn two_quick_publishes_leave_the_newest_record() {
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
    let latest = resign("finished");
    assert!(first.body.version < latest.body.version);
    b.node.publish(first);
    b.node.publish(latest.clone());
    wait(|| {
        a.db.lock()
            .unwrap()
            .computer_records()
            .unwrap()
            .contains(&latest)
    });
    for _ in 0..20 {
        assert!(
            a.db.lock()
                .unwrap()
                .computer_records()
                .unwrap()
                .contains(&latest)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn quick_publishes_share_one_publisher() {
    let (a, b) = joined();
    let records: Vec<_> = (0..20)
        .map(|index| {
            b.db.lock()
                .unwrap()
                .resign(
                    &b.key,
                    "birch",
                    &b.node.addrs(),
                    "lantern",
                    Some(["ready", "finished"][index % 2]),
                    None,
                    now(),
                )
                .unwrap()
        })
        .collect();
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].body.version < pair[1].body.version)
    );
    let latest = records.last().unwrap().clone();
    for record in records {
        b.node.publish(record);
    }
    wait(|| {
        a.db.lock()
            .unwrap()
            .computer_records()
            .unwrap()
            .contains(&latest)
    });
    assert!((1..=3).contains(&b.node.published()));
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
