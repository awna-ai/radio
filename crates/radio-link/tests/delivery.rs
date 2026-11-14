mod common;

use common::{SHA, bounded};
use radio_link::{
    SecretKey,
    admit::Limits,
    code::{Hints, JoinCode},
    fleet_db::{FleetDb, State},
    link::Local,
    node::Node,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
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
        Self::with_limits(name, Limits::default())
    }

    fn with_limits(name: &str, limits: Limits) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "radio-delivery-{}-{}",
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
                clients: Some(dir.join("clients")),
            },
            limits,
        )
        .unwrap();
        Self {
            node,
            key,
            db,
            dir: Temp(dir),
        }
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

fn folder(computer: &Computer) -> PathBuf {
    let path = computer.dir.0.join("inbox");
    fs::create_dir_all(computer.dir.0.join("clients")).unwrap();
    fs::write(computer.dir.0.join("clients/notes.json"), b"{}").unwrap();
    computer
        .db
        .lock()
        .unwrap()
        .set_delivery_folder("notes", &path)
        .unwrap();
    path
}
fn source(computer: &Computer, bytes: &[u8]) -> PathBuf {
    let path = computer.dir.0.join("source");
    fs::write(&path, bytes).unwrap();
    path
}
fn no_arrivals(computer: &Computer) {
    assert!(
        computer
            .db
            .lock()
            .unwrap()
            .arrivals("notes", 0, 10)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn a_mebibyte_arrives_with_private_mode_and_sender_name() {
    let (a, b) = joined();
    let inbox = folder(&a);
    let bytes: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    let path = source(&b, &bytes);
    assert_eq!(
        b.node.send_file("garden", "notes", "letter", &path),
        Ok(bytes.len() as u64)
    );
    assert_eq!(fs::read(inbox.join("letter")).unwrap(), bytes);
    assert_eq!(
        fs::metadata(inbox.join("letter"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    wait(|| {
        !a.db
            .lock()
            .unwrap()
            .arrivals("notes", 0, 10)
            .unwrap()
            .is_empty()
    });
    let rows = a.db.lock().unwrap().arrivals("notes", 0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, b.node.id().to_string());
    assert_eq!(
        (&*rows[0].computer, &*rows[0].name, rows[0].size),
        ("birch", "letter", bytes.len() as u64)
    );
}
#[test]
fn dropping_an_unstarted_send_delivers_nothing() {
    let (a, b) = joined();
    let inbox = folder(&a);
    let path = source(&b, b"hello");
    let pending = b
        .node
        .start_send("garden", "notes", "letter", &path)
        .unwrap();
    drop(pending);
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
        no_arrivals(&a);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_unpaired_computer_cannot_deliver() {
    let (a, b) = joined();
    let inbox = folder(&a);
    runtime().block_on(bounded(async {
        let peer = b
            .node
            .link()
            .dial(a.node.id(), &a.node.addrs())
            .await
            .unwrap();
        let (mut send, mut recv) = peer.connection().open_bi().await.unwrap();
        let mut head = Vec::new();
        radio_link::frame::write(
            &mut head,
            &radio_link::wire::Request::File {
                to: a.node.id().to_string(),
                app: "notes".into(),
                name: "letter".into(),
                size: 5,
            },
            radio_link::frame::CONTROL_CAP,
        )
        .await
        .unwrap();
        // Admit this stream while accepted, but keep its head incomplete.
        send.write_all(&head[..1]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        a.db.lock()
            .unwrap()
            .block(&b.node.id().to_string())
            .unwrap();
        assert_eq!(a.state(&b), Some(State::Blocked));
        send.write_all(&head[1..]).await.unwrap();
        send.write_all(b"hello").await.unwrap();
        send.finish().unwrap();
        let result = radio_link::frame::read::<radio_link::wire::Answer, _>(
            &mut recv,
            radio_link::frame::CONTROL_CAP,
        )
        .await;
        assert!(
            result.is_err(),
            "the receiver accepted a blocked computer: {result:?}"
        );
    }));
    assert_eq!(fs::read_dir(inbox).unwrap().count(), 0);
    no_arrivals(&a);
}
#[test]
fn an_oversize_file_is_refused_before_lookup_or_dial() {
    let a = Computer::new("garden");
    let path = source(&a, b"");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(radio_link::wire::FILE_CAP + 1)
        .unwrap();
    assert_eq!(
        a.node.send_file("unknown", "notes", "letter", &path),
        Err("the file is larger than 64 MiB")
    );
}
#[test]
fn an_unregistered_app_takes_no_file() {
    let (a, b) = joined();
    let path = source(&b, b"hello");
    assert_eq!(
        b.node.send_file("garden", "notes", "letter", &path),
        Err("that app takes no files on that computer")
    );
    no_arrivals(&a);
}
#[test]
fn invalid_names_are_refused_before_dial() {
    let a = Computer::new("garden");
    let path = source(&a, b"hello");
    for name in ["../x", "a/b", ".hidden", "", &"x".repeat(256), "line\nfeed"] {
        assert_eq!(
            a.node.send_file("unknown", "notes", name, &path),
            Err("not a file name"),
            "{name:?}"
        );
    }
}
#[test]
fn a_swapped_folder_link_is_never_followed() {
    let (a, b) = joined();
    let inbox = folder(&a);
    let target = a.dir.0.join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir(&inbox).unwrap();
    symlink(&target, &inbox).unwrap();
    let path = source(&b, b"hello");
    assert_eq!(
        b.node.send_file("garden", "notes", "letter", &path),
        Err("that app's folder on that computer is not usable")
    );
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    no_arrivals(&a);
}
#[test]
fn an_existing_file_is_never_replaced() {
    let (a, b) = joined();
    let inbox = folder(&a);
    let path = source(&b, b"first");
    assert_eq!(b.node.send_file("garden", "notes", "letter", &path), Ok(5));
    fs::write(&path, b"other").unwrap();
    assert_eq!(
        b.node.send_file("garden", "notes", "letter", &path),
        Err("a file with that name is already there")
    );
    assert_eq!(fs::read(inbox.join("letter")).unwrap(), b"first");
}
#[test]
fn a_source_link_or_directory_is_refused() {
    let a = Computer::new("garden");
    let path = source(&a, b"hello");
    let link = a.dir.0.join("source-link");
    symlink(&path, &link).unwrap();
    assert_eq!(
        a.node.send_file("unknown", "notes", "letter", &link),
        Err("the file could not be opened")
    );
    assert_eq!(
        a.node.send_file("unknown", "notes", "letter", &a.dir.0),
        Err("not a regular file")
    );
}

#[test]
fn a_source_fifo_is_refused_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let a = Computer::new("garden");
    let path = a.dir.0.join("pipe");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and valid for the call.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sent.send(a.node.send_file("unknown", "notes", "letter", &path))
            .unwrap();
    });
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)),
        Ok(Err("not a regular file"))
    );
    worker.join().unwrap();
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn oversized_and_path_heads_are_reset() {
    for (name, size) in [("letter", radio_link::wire::FILE_CAP + 1), ("../x", 0)] {
        let (a, b) = joined();
        let inbox = folder(&a);
        runtime().block_on(bounded(async {
            let peer = b.node.link().dial(a.node.id(),&a.node.addrs()).await.unwrap();
            let (mut send,mut recv) = peer.connection().open_bi().await.unwrap();
            radio_link::frame::write(&mut send,&radio_link::wire::Request::File {
                to:a.node.id().to_string(),app:"notes".into(),name:name.into(),size
            },radio_link::frame::CONTROL_CAP).await.unwrap();
            send.finish().unwrap();
            assert!(matches!(recv.read_chunk(1).await,Err(iroh::endpoint::ReadError::Reset(code)) if code == radio_link::link::BAD_FRAME.into()));
        }));
        assert_eq!(fs::read_dir(inbox).unwrap().count(), 0);
        no_arrivals(&a);
    }
}

#[test]
fn short_and_long_bodies_are_reset_without_leaving_files() {
    for body in [b"ab".as_slice(), b"abcd".as_slice()] {
        let (a, b) = joined();
        let inbox = folder(&a);
        runtime().block_on(bounded(async {
            let peer = b.node.link().dial(a.node.id(),&a.node.addrs()).await.unwrap();
            let (mut send,mut recv) = peer.connection().open_bi().await.unwrap();
            radio_link::frame::write(&mut send,&radio_link::wire::Request::File {
                to:a.node.id().to_string(),app:"notes".into(),name:"letter".into(),size:3
            },radio_link::frame::CONTROL_CAP).await.unwrap();
            send.write_all(body).await.unwrap();
            send.finish().unwrap();
            assert!(matches!(recv.read_chunk(1).await,Err(iroh::endpoint::ReadError::Reset(code)) if code == radio_link::link::BAD_FRAME.into()));
        }));
        assert_eq!(fs::read_dir(inbox).unwrap().count(), 0);
        no_arrivals(&a);
    }
}

#[test]
fn a_held_body_makes_the_next_delivery_busy() {
    let a = Computer::with_limits(
        "garden",
        Limits {
            deliveries: 4,
            ..Limits::default()
        },
    );
    let b = Computer::new("birch");
    a.found();
    assert_eq!(b.node.join(a.code(), now()), Ok(()));
    wait(|| a.state(&b) == Some(State::Accepted));
    let inbox = folder(&a);
    let (written, ready) = std::sync::mpsc::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    std::thread::scope(|scope| {
        let a = &a;
        let b = &b;
        let thread = scope.spawn(move || {
            runtime().block_on(bounded(async {
                let peer = b
                    .node
                    .link()
                    .dial(a.node.id(), &a.node.addrs())
                    .await
                    .unwrap();
                let (mut send, _recv) = peer.connection().open_bi().await.unwrap();
                radio_link::frame::write(
                    &mut send,
                    &radio_link::wire::Request::File {
                        to: a.node.id().to_string(),
                        app: "notes".into(),
                        name: "held".into(),
                        size: 3,
                    },
                    radio_link::frame::CONTROL_CAP,
                )
                .await
                .unwrap();
                send.write_all(b"a").await.unwrap();
                written.send(()).unwrap();
                released.await.unwrap();
                send.reset(0u32.into()).unwrap();
                drop(peer);
            }));
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        wait(|| fs::read_dir(&inbox).unwrap().count() == 1);
        let path = source(b, b"other");
        assert_eq!(
            b.node.send_file("garden", "notes", "letter", &path),
            Err("that computer is busy; try again")
        );
        release.send(()).unwrap();
        thread.join().unwrap();
    });
    wait(|| fs::read_dir(&inbox).unwrap().count() == 0);
    no_arrivals(&a);
}

#[test]
fn an_unreachable_computer_fails_within_the_dial_deadline() {
    let a = Computer::with_limits(
        "garden",
        Limits {
            dial: Duration::from_millis(100),
            ..Limits::default()
        },
    );
    let b = Computer::new("birch");
    a.found();
    assert_eq!(b.node.join(a.code(), now()), Ok(()));
    wait(|| a.state(&b) == Some(State::Accepted));
    let oak = Computer::new("oak");
    assert_eq!(oak.node.join(a.code(), now()), Ok(()));
    wait(|| a.state(&oak) == Some(State::Accepted));
    drop(oak);
    let path = source(&a, b"hello");
    let start = Instant::now();
    assert_eq!(
        a.node.send_file("oak", "notes", "letter", &path),
        Err("the computer could not be reached")
    );
    assert!(start.elapsed() < Duration::from_millis(2100));
}

#[test]
fn an_app_without_a_client_record_takes_no_file() {
    let (a, b) = joined();
    let inbox = folder(&a);
    fs::remove_file(a.dir.0.join("clients/notes.json")).unwrap();
    let path = source(&b, b"hello");
    assert_eq!(
        b.node.send_file("garden", "notes", "report.txt", &path),
        Err("that app takes no files on that computer")
    );
    assert_eq!(fs::read_dir(inbox).unwrap().count(), 0);
    no_arrivals(&a);
}

#[test]
fn a_full_folder_takes_no_file() {
    let (a, b) = joined();
    let inbox = folder(&a);
    for i in 0..64 {
        fs::write(inbox.join(format!("report-{i}.txt")), b"").unwrap();
    }
    let path = source(&b, b"hello");
    assert_eq!(
        b.node.send_file("garden", "notes", "report.txt", &path),
        Err("that app's folder on that computer is full")
    );
    assert_eq!(fs::read_dir(inbox).unwrap().count(), 64);
    no_arrivals(&a);
}

#[test]
fn a_stalled_body_is_reset_and_removed() {
    let a = Computer::with_limits(
        "garden",
        Limits {
            stall: Duration::from_millis(80),
            ..Limits::default()
        },
    );
    let b = Computer::new("birch");
    a.found();
    b.node.join(a.code(), now()).unwrap();
    wait(|| a.state(&b) == Some(State::Accepted));
    let inbox = folder(&a);
    runtime().block_on(bounded(async {
        let peer = b.node.link().dial(a.node.id(), &a.node.addrs()).await.unwrap();
        let (mut send, mut recv) = peer.connection().open_bi().await.unwrap();
        radio_link::frame::write(&mut send, &radio_link::wire::Request::File { to: a.node.id().to_string(), app: "notes".into(), name: "report.txt".into(), size: 3 }, radio_link::frame::CONTROL_CAP).await.unwrap();
        send.write_all(b"a").await.unwrap();
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_millis(600), recv.read_chunk(1)).await;
        assert!(matches!(result, Ok(Err(iroh::endpoint::ReadError::Reset(code))) if code == radio_link::link::BAD_FRAME.into()));
        assert!(started.elapsed() < Duration::from_millis(600));
    }));
    wait(|| fs::read_dir(&inbox).unwrap().count() == 0);
    no_arrivals(&a);
}

#[test]
fn a_body_finishing_near_its_deadline_is_answered() {
    let a = Computer::with_limits(
        "garden",
        Limits {
            file: Duration::from_millis(250),
            ..Limits::default()
        },
    );
    let b = Computer::new("birch");
    a.found();
    b.node.join(a.code(), now()).unwrap();
    wait(|| a.state(&b) == Some(State::Accepted));
    let inbox = folder(&a);
    runtime().block_on(bounded(async {
        let peer = b
            .node
            .link()
            .dial(a.node.id(), &a.node.addrs())
            .await
            .unwrap();
        let (mut send, mut recv) = peer.connection().open_bi().await.unwrap();
        radio_link::frame::write(
            &mut send,
            &radio_link::wire::Request::File {
                to: a.node.id().to_string(),
                app: "notes".into(),
                name: "report.txt".into(),
                size: 3,
            },
            radio_link::frame::CONTROL_CAP,
        )
        .await
        .unwrap();
        send.write_all(b"a").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        send.write_all(b"bc").await.unwrap();
        send.finish().unwrap();
        assert_eq!(
            radio_link::frame::read::<radio_link::wire::Answer, _>(
                &mut recv,
                radio_link::frame::CONTROL_CAP
            )
            .await,
            Ok(radio_link::wire::Answer::Ok)
        );
    }));
    assert_eq!(fs::read(inbox.join("report.txt")).unwrap(), b"abc");
}

#[test]
fn two_keys_can_deliver_overlapping_bodies() {
    let (a, b) = joined();
    let c = Computer::new("cedar");
    c.node.join(a.code(), now()).unwrap();
    wait(|| a.state(&c) == Some(State::Accepted));
    let inbox = folder(&a);
    runtime().block_on(bounded(async {
        let first = b
            .node
            .link()
            .dial(a.node.id(), &a.node.addrs())
            .await
            .unwrap();
        let second = c
            .node
            .link()
            .dial(a.node.id(), &a.node.addrs())
            .await
            .unwrap();
        let (mut first_send, mut first_recv) = first.connection().open_bi().await.unwrap();
        let (mut second_send, mut second_recv) = second.connection().open_bi().await.unwrap();
        for (send, name) in [(&mut first_send, "one.txt"), (&mut second_send, "two.txt")] {
            radio_link::frame::write(
                send,
                &radio_link::wire::Request::File {
                    to: a.node.id().to_string(),
                    app: "notes".into(),
                    name: name.into(),
                    size: 3,
                },
                radio_link::frame::CONTROL_CAP,
            )
            .await
            .unwrap();
            send.write_all(b"a").await.unwrap();
        }
        while fs::read_dir(&inbox).unwrap().count() != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        first_send.write_all(b"bc").await.unwrap();
        second_send.write_all(b"de").await.unwrap();
        first_send.finish().unwrap();
        second_send.finish().unwrap();
        for recv in [&mut first_recv, &mut second_recv] {
            assert_eq!(
                radio_link::frame::read::<radio_link::wire::Answer, _>(
                    recv,
                    radio_link::frame::CONTROL_CAP
                )
                .await,
                Ok(radio_link::wire::Answer::Ok)
            );
        }
    }));
    assert_eq!(fs::read(inbox.join("one.txt")).unwrap(), b"abc");
    assert_eq!(fs::read(inbox.join("two.txt")).unwrap(), b"ade");
    wait(|| a.db.lock().unwrap().arrivals("notes", 0, 10).unwrap().len() == 2);
    let rows = a.db.lock().unwrap().arrivals("notes", 0, 10).unwrap();
    assert!(
        rows.iter()
            .any(|row| row.name == "one.txt" && row.key == b.node.id().to_string())
    );
    assert!(
        rows.iter()
            .any(|row| row.name == "two.txt" && row.key == c.node.id().to_string())
    );
}
