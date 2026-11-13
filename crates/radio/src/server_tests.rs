use super::*;
use radio_client::{Member, REQUEST_MAX_BYTES, Request, Response, request, socket_path};
use radio_rooms::{Author, RoomStore};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::Instant,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/tmp/rd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn socket(&self) -> PathBuf {
        self.0.join("radio/radio.sock")
    }

    fn db(&self) -> PathBuf {
        self.0.join("rooms/rooms.db")
    }

    fn bind(&self) -> Server {
        Server::bind(&self.socket(), &self.db(), "test-host".into()).unwrap()
    }

    fn reader(&self) -> RoomStore {
        RoomStore::open(&self.db()).unwrap()
    }

    fn create(&self) {
        assert!(request(&self.socket(), &create()).unwrap().ok);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Finite accept loops keep test threads and store handles from outliving their fixtures.
fn start(mut server: Server, connections: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        for _ in 0..connections {
            let (stream, _) = server.listener.accept().unwrap();
            server.serve_one(stream);
        }
    })
}

fn create() -> Request {
    Request::CreateRoom {
        room: "garden".into(),
        title: "Garden".into(),
    }
}

fn post(body: &str) -> Request {
    Request::Post {
        room: "garden".into(),
        author: Member {
            kind: "human".into(),
            name: "sam".into(),
        },
        body: body.into(),
    }
}

// macOS refuses socket options once the server has answered and closed (EINVAL); the answer is still buffered.
pub(super) fn read_timeout(stream: &UnixStream, timeout: Duration) {
    if let Err(error) = stream.set_read_timeout(Some(timeout)) {
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
    }
}

// The server may answer and close before the request is half-closed.
pub(super) fn half_close(stream: &UnixStream) {
    if let Err(error) = stream.shutdown(std::net::Shutdown::Write) {
        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected, "{error}");
    }
}

#[test]
fn test_clients_read_an_answer_sent_before_the_close() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"ok\":true}\n").unwrap();
    drop(peer);

    read_timeout(&stream, Duration::from_secs(1));
    half_close(&stream);
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    assert_eq!(line, "{\"ok\":true}\n");
}

fn raw_value(socket: &Path, line: &[u8]) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    read_timeout(&stream, Duration::from_secs(5));
    stream.write_all(line).unwrap();
    half_close(&stream);
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

fn raw(socket: &Path, line: &[u8]) -> Response {
    serde_json::from_value(raw_value(socket, line)).unwrap()
}

#[test]
fn path_and_permissions_are_private() {
    let fixture = Fixture::new();
    assert_eq!(
        socket_path(&fixture.0),
        fixture.0.join(".local/state/radio/radio.sock")
    );
    let server = fixture.bind();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&fixture.socket()), 0o600);
    assert_eq!(mode(fixture.socket().parent().unwrap()), 0o700);
    let worker = start(server, 1);
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn stale_socket_is_replaced() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.socket().parent().unwrap()).unwrap();
    drop(UnixListener::bind(fixture.socket()).unwrap());
    let worker = start(fixture.bind(), 1);
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn bound_server_keeps_its_socket_until_dropped() {
    let fixture = Fixture::new();
    let server = fixture.bind();
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "other-host".into(),)
            .err()
            .unwrap(),
        "another Radio serves radio.sock"
    );
    assert!(
        fs::symlink_metadata(fixture.socket())
            .unwrap()
            .file_type()
            .is_socket()
    );
    drop(server);
    let worker = start(fixture.bind(), 1);
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn racing_binds_leave_one_reachable_server() {
    let fixture = Fixture::new();
    let barrier = std::sync::Barrier::new(2);
    let results = thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    Server::bind(&fixture.socket(), &fixture.db(), "test-host".into())
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap(),
        "another Radio serves radio.sock"
    );
    let server = results.into_iter().find_map(Result::ok).unwrap();
    let worker = start(server, 1);
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn bind_recreates_a_lock_removed_by_its_previous_owner() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.socket().parent().unwrap()).unwrap();
    let path = fixture.socket().with_extension("sock.lock");
    let file = fs::File::create(&path).unwrap();
    // SAFETY: file is live; flock has no pointer preconditions.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
    fs::remove_file(&path).unwrap();
    drop(file);
    let _server = fixture.bind();
    assert!(path.exists());
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn live_socket_is_refused_and_the_original_server_keeps_answering() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 1);
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "other-host".into(),)
            .err()
            .unwrap(),
        "another Radio serves radio.sock"
    );
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn files_and_symlinks_are_refused_and_preserved() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.socket().parent().unwrap()).unwrap();
    fs::write(fixture.socket(), "keep").unwrap();
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "test-host".into(),)
            .err()
            .unwrap(),
        "radio.sock is not a socket"
    );
    assert_eq!(fs::read(fixture.socket()).unwrap(), b"keep");
    fs::remove_file(fixture.socket()).unwrap();
    let target = fixture.0.join("target");
    fs::write(&target, "keep").unwrap();
    symlink(&target, fixture.socket()).unwrap();
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "test-host".into(),)
            .err()
            .unwrap(),
        "radio.sock is not a socket"
    );
    assert!(
        fs::symlink_metadata(fixture.socket())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn posts_get_sequences_and_server_owned_host_and_time() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 3);
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    fixture.create();
    for (body, seq) in [("first", 1), ("second", 2)] {
        let response = request(&fixture.socket(), &post(body)).unwrap();
        assert!(response.ok);
        assert_eq!(response.seq, Some(seq));
        assert!(response.error.is_none());
    }
    worker.join().unwrap();
    let reader = fixture.reader();
    let messages = reader.tail("garden", 10).unwrap();
    assert_eq!(messages.len(), 2);
    for (message, body) in messages.iter().zip(["first", "second"]) {
        assert_eq!(message.author_host, "test-host");
        assert_eq!(message.author, Author::human("sam"));
        assert_eq!(message.body, body);
        assert!(message.sent_at >= before);
    }
    let room = reader.room("garden").unwrap().unwrap();
    assert_eq!(room.home, "test-host");
}

#[test]
fn status_set_and_clear_report_the_removed_key() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 4);
    fixture.create();
    assert!(
        request(
            &fixture.socket(),
            &Request::StatusSet {
                room: "garden".into(),
                key: "work".into(),
                line: "Ready".into(),
                author: Member {
                    kind: "human".into(),
                    name: "sam".into()
                },
                expires_at: None,
            }
        )
        .unwrap()
        .ok
    );
    assert_eq!(
        fixture.reader().status("garden", 0.0).unwrap()[0].line,
        "Ready"
    );
    let clear = Request::StatusClear {
        room: "garden".into(),
        key: "work".into(),
        author: Member {
            kind: "human".into(),
            name: "sam".into(),
        },
    };
    let response = request(&fixture.socket(), &clear).unwrap();
    assert!(response.ok);
    assert_eq!(response.removed, Some(true));
    assert_eq!(
        request(&fixture.socket(), &clear).unwrap().removed,
        Some(false)
    );
    assert!(fixture.reader().status("garden", 0.0).unwrap().is_empty());
    worker.join().unwrap();
}

#[test]
fn invalid_requests_return_fixed_errors_and_server_keeps_accepting() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 7);
    let mut oversized = vec![b'x'; REQUEST_MAX_BYTES + 1];
    oversized.push(b'\n');
    for (line, error) in [
        (oversized.as_slice(), "the request is too long"),
        (b"private text\n".as_slice(), "not a request"),
        (b"{\"v\":1,\"op\":\"unknown\"}\n".as_slice(), "not a request"),
        (b"{\"v\":1,\"op\":\"create_room\",\"room\":\"garden\",\"title\":\"Garden\",\"host\":\"forged\"}\n".as_slice(), "not a request"),
        (b"{\"v\":1,\"op\":\"post\",\"room\":\"garden\",\"author\":{\"kind\":\"human\",\"name\":\"sam\"},\"body\":\"hi\",\"now\":0}\n".as_slice(), "not a request"),
    ] {
        let response = raw(&fixture.socket(), line);
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some(error));
        assert!(response.seq.is_none());
    }
    let response = request(&fixture.socket(), &post("private text")).unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.as_deref(), Some("no such room"));
    fixture.create();
    worker.join().unwrap();
}

#[test]
fn request_size_limit_includes_the_newline() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 2);
    let mut request = Request::CreateRoom {
        room: "garden".into(),
        title: String::new(),
    };
    let overhead = wire(&request).len() + 1;
    let Request::CreateRoom { title, .. } = &mut request else {
        unreachable!()
    };
    *title = "x".repeat(REQUEST_MAX_BYTES - overhead);
    let mut exact = wire(&request);
    exact.push(b'\n');
    assert_eq!(exact.len(), REQUEST_MAX_BYTES);
    assert!(raw(&fixture.socket(), &exact).ok);

    exact.insert(exact.len() - 3, b'x');
    assert_eq!(exact.len(), REQUEST_MAX_BYTES + 1);
    let response = raw(&fixture.socket(), &exact);
    assert!(!response.ok);
    assert_eq!(response.error.as_deref(), Some("the request is too long"));
    worker.join().unwrap();
}

#[test]
fn slow_request_hits_the_deadline_and_server_keeps_accepting() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind().with_deadline(Duration::from_millis(800)), 2);
    let connected = Instant::now();
    let stream = UnixStream::connect(fixture.socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let trickle = thread::spawn(move || {
        for _ in 0..10 {
            let _ = writer.write_all(b"x");
            thread::sleep(Duration::from_millis(200));
        }
    });
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    assert!(connected.elapsed() < Duration::from_millis(1500));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap(),
        serde_json::json!({"ok": false, "error": "the request took too long"})
    );
    fixture.create();
    worker.join().unwrap();
    trickle.join().unwrap();
}

#[test]
fn wrong_uid_is_refused_without_reading_or_writing_a_post() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 1);
    fixture.create();
    worker.join().unwrap();
    // SAFETY: geteuid has no arguments or memory preconditions.
    let own_uid = unsafe { libc::geteuid() };
    let worker = start(fixture.bind().with_uid(own_uid + 1), 2);
    let response = request(&fixture.socket(), &post("private text")).unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.as_deref(), Some("refused: another user"));
    // No request bytes: the peer check must answer before attempting to read.
    let mut response = String::new();
    let stream = UnixStream::connect(fixture.socket()).unwrap();
    read_timeout(&stream, Duration::from_secs(1));
    BufReader::new(stream).read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<Response>(&response)
            .unwrap()
            .error
            .as_deref(),
        Some("refused: another user")
    );
    worker.join().unwrap();
    assert!(fixture.reader().tail("garden", 10).unwrap().is_empty());
}

#[test]
fn missing_server_is_reported() {
    let fixture = Fixture::new();
    assert_eq!(
        request(&fixture.socket(), &post("hi")).err().unwrap(),
        "Radio is not running"
    );
}

#[test]
fn post_is_served_while_an_unrelated_thread_is_busy() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 2);
    fixture.create();
    let (started, busy) = std::sync::mpsc::channel();
    let busy_thread = thread::spawn(move || {
        started.send(()).unwrap();
        thread::sleep(Duration::from_secs(2));
    });
    busy.recv().unwrap();
    let start = Instant::now();
    assert!(request(&fixture.socket(), &post("hi")).unwrap().ok);
    assert!(start.elapsed() < Duration::from_secs(1));
    worker.join().unwrap();
    busy_thread.join().unwrap();
}

fn wire(request: &Request) -> Vec<u8> {
    let mut value = serde_json::to_value(request).unwrap();
    value["v"] = serde_json::json!(1);
    serde_json::to_vec(&value).unwrap()
}

fn call(socket: &Path, value: serde_json::Value) -> serde_json::Value {
    let mut line = serde_json::to_vec(&value).unwrap();
    line.push(b'\n');
    raw_value(socket, &line)
}

#[test]
fn versions_are_required_and_checked_before_the_op() {
    let fixture = Fixture::new();
    let cases = [
        serde_json::json!({"op":"hello"}),
        serde_json::json!({"v":"1","op":"hello"}),
        serde_json::json!({"v":-1,"op":"hello"}),
        serde_json::json!({"v":1.5,"op":"hello"}),
        serde_json::json!({"v":4294967296u64,"op":"hello"}),
        serde_json::json!([]),
        serde_json::json!("hello"),
    ];
    let worker = start(fixture.bind(), cases.len() + 3);
    for value in cases {
        assert_eq!(
            call(&fixture.socket(), value),
            serde_json::json!({"ok":false,"error":"not a request"})
        );
    }
    for value in [
        serde_json::json!({"v":99,"op":"hello"}),
        serde_json::json!({"v":2,"op":"hello"}),
        serde_json::json!({"v":2,"op":"nonsense"}),
    ] {
        assert_eq!(
            call(&fixture.socket(), value),
            serde_json::json!({"ok":false,"error":"unsupported_version","supported":[1]})
        );
    }
    worker.join().unwrap();
}

#[test]
fn hello_names_the_version_and_all_ops() {
    assert!(radio_client::OPS.contains(&"id"));
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 1);
    assert_eq!(
        call(&fixture.socket(), serde_json::json!({"v":1,"op":"hello"})),
        serde_json::json!({
            "ok":true,"radio":env!("CARGO_PKG_VERSION"),"protocol":{"major":1,"minor":3},"majors":[1],"ops":radio_client::OPS
        })
    );
    worker.join().unwrap();
}

fn op_samples() -> Vec<serde_json::Value> {
    let author = serde_json::json!({"kind":"human","name":"sam"});
    vec![
        serde_json::json!({"op":"hello"}),
        serde_json::json!({"op":"register","app":"lantern","app_version":"1.0.0","members":[]}),
        serde_json::json!({"op":"create_room","room":"garden","title":"Garden"}),
        serde_json::json!({"op":"post","room":"garden","author":author,"body":"hi"}),
        serde_json::json!({"op":"status_set","room":"garden","author":author,"key":"mood","line":"Ready"}),
        serde_json::json!({"op":"status_clear","room":"garden","author":author,"key":"mood"}),
        serde_json::json!({"op":"list_rooms"}),
        serde_json::json!({"op":"read","room":"garden"}),
        serde_json::json!({"op":"status","room":"garden"}),
    ]
}

#[test]
fn unknown_fields_are_refused_on_every_op_and_member() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 10);
    for mut value in op_samples() {
        assert!(serde_json::from_value::<Request>(value.clone()).is_ok());
        value["x"] = serde_json::json!(1);
        assert!(
            serde_json::from_value::<Request>(value.clone()).is_err(),
            "{value}"
        );
        value["v"] = serde_json::json!(1);
        assert_eq!(
            call(&fixture.socket(), value),
            serde_json::json!({"ok":false,"error":"not a request"})
        );
    }
    let mut value = serde_json::json!({"v":1,"op":"post","room":"garden","author":{"kind":"human","name":"sam","x":1},"body":"hi"});
    assert_eq!(
        call(&fixture.socket(), value.take()),
        serde_json::json!({"ok":false,"error":"not a request"})
    );
    worker.join().unwrap();
}

#[test]
fn bind_makes_the_home_room_once() {
    let fixture = Fixture::new();
    drop(fixture.bind());
    drop(fixture.bind());
    let rooms = fixture.reader().rooms(None, 500).unwrap();
    assert_eq!(rooms.len(), 1);
    assert_eq!(
        (
            &*rooms[0].id,
            &*rooms[0].home,
            &*rooms[0].title,
            rooms[0].last_seq
        ),
        ("test-host", "test-host", "Room on test-host", 0)
    );
}

fn register(members: Vec<Member>) -> Request {
    Request::Register {
        app: "lantern".into(),
        app_version: "1.0.0".into(),
        members,
    }
}
fn helper() -> Member {
    Member {
        kind: "helper".into(),
        name: "juniper".into(),
    }
}

#[test]
fn membership_is_checked_afresh_for_every_write() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 10);
    fixture.create();
    let socket = fixture.socket();
    let member_post = Request::Post {
        room: "garden".into(),
        author: helper(),
        body: "hi".into(),
    };
    let set = Request::StatusSet {
        room: "garden".into(),
        key: "mood".into(),
        line: "Ready".into(),
        author: helper(),
        expires_at: None,
    };
    let clear = Request::StatusClear {
        room: "garden".into(),
        key: "mood".into(),
        author: helper(),
    };
    let refused = request(&socket, &member_post).unwrap();
    assert_eq!(
        (refused.error.as_deref(), refused.refused),
        (Some("not a member"), Some(true))
    );
    assert!(request(&socket, &register(vec![helper()])).unwrap().ok);
    assert_eq!(request(&socket, &member_post).unwrap().seq, Some(1));
    assert_eq!(request(&socket, &set).unwrap().seq, Some(2));
    assert!(request(&socket, &register(vec![])).unwrap().ok);
    for op in [&member_post, &set, &clear] {
        let refused = request(&socket, op).unwrap();
        assert_eq!(
            (refused.error.as_deref(), refused.refused),
            (Some("not a member"), Some(true))
        );
    }
    assert_eq!(request(&socket, &post("hello")).unwrap().seq, Some(3));
    worker.join().unwrap();
    assert_eq!(fixture.reader().tail("garden", 10).unwrap().len(), 3);
}

#[test]
fn ownership_refusals_and_clear_sequences() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 6);
    fixture.create();
    assert!(
        request(&fixture.socket(), &register(vec![helper()]))
            .unwrap()
            .ok
    );
    let mut set = Request::StatusSet {
        room: "garden".into(),
        key: "mood".into(),
        line: "Ready".into(),
        author: Member {
            kind: "human".into(),
            name: "sam".into(),
        },
        expires_at: Some(f64::MAX),
    };
    assert_eq!(request(&fixture.socket(), &set).unwrap().seq, Some(1));
    if let Request::StatusSet { author, .. } = &mut set {
        *author = helper();
    }
    let refused = request(&fixture.socket(), &set).unwrap();
    assert_eq!(
        (refused.error.as_deref(), refused.refused),
        (Some(radio_rooms::OWNED), Some(true))
    );
    let refused = request(
        &fixture.socket(),
        &Request::StatusClear {
            room: "garden".into(),
            key: "mood".into(),
            author: helper(),
        },
    )
    .unwrap();
    assert_eq!(
        (refused.error.as_deref(), refused.refused),
        (Some(radio_rooms::OWNED), Some(true))
    );
    let cleared = request(
        &fixture.socket(),
        &Request::StatusClear {
            room: "garden".into(),
            key: "mood".into(),
            author: Member {
                kind: "human".into(),
                name: "ada".into(),
            },
        },
    )
    .unwrap();
    assert_eq!((cleared.removed, cleared.seq), (Some(true), Some(2)));
    worker.join().unwrap();
}

#[test]
fn reads_have_defaults_limits_room_checks_and_live_status() {
    let fixture = Fixture::new();
    let mut server = fixture.bind();
    server
        .store
        .create_room("garden", "test-host", "Garden", now().unwrap())
        .unwrap();
    for _ in 0..501 {
        server
            .store
            .post(
                "garden",
                "test-host",
                &Author::human("sam"),
                "hi",
                now().unwrap(),
            )
            .unwrap();
    }
    server
        .store
        .set_status(
            "garden",
            "plan",
            "Ready",
            "test-host",
            &Author::human("sam"),
            None,
            now().unwrap(),
        )
        .unwrap();
    server
        .store
        .set_status(
            "garden",
            "old",
            "Expired",
            "test-host",
            &Author::human("sam"),
            Some(2.0),
            1.0,
        )
        .unwrap();
    let worker = start(server, 10);
    let socket = fixture.socket();
    let rooms = request(
        &socket,
        &Request::ListRooms {
            after: None,
            limit: None,
        },
    )
    .unwrap()
    .rooms
    .unwrap();
    assert_eq!(rooms.len(), 2);
    assert_eq!((&*rooms[0].id, rooms[0].last_seq), ("garden", 503));
    assert_eq!(
        (
            &*rooms[1].id,
            &*rooms[1].home,
            &*rooms[1].title,
            rooms[1].last_seq
        ),
        ("test-host", "test-host", "Room on test-host", 0)
    );
    let read = |since, limit| Request::Read {
        room: "garden".into(),
        since,
        limit,
    };
    let tail = request(&socket, &read(None, None))
        .unwrap()
        .messages
        .unwrap();
    assert_eq!((tail.len(), tail[0].seq, tail[49].seq), (50, 454, 503));
    let since = request(&socket, &read(Some(0), None))
        .unwrap()
        .messages
        .unwrap();
    assert_eq!((since.len(), since[0].seq, since[499].seq), (500, 1, 500));
    let last = request(&socket, &read(Some(501), Some(1)))
        .unwrap()
        .messages
        .unwrap();
    assert_eq!((last.len(), last[0].seq), (1, 502));
    let tail = request(&socket, &read(None, Some(2)))
        .unwrap()
        .messages
        .unwrap();
    assert_eq!((tail.len(), tail[0].seq, tail[1].seq), (2, 502, 503));
    for limit in [0, 501] {
        assert_eq!(
            request(&socket, &read(None, Some(limit)))
                .unwrap()
                .error
                .as_deref(),
            Some("not a limit")
        );
    }
    let status = request(
        &socket,
        &Request::Status {
            room: "garden".into(),
        },
    )
    .unwrap()
    .status
    .unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].key, "plan");
    for op in [
        Request::Read {
            room: "missing".into(),
            since: None,
            limit: None,
        },
        Request::Status {
            room: "missing".into(),
        },
    ] {
        assert_eq!(
            request(&socket, &op).unwrap().error.as_deref(),
            Some("no such room")
        );
    }
    worker.join().unwrap();
}

#[test]
fn largest_escaped_read_round_trips_through_client() {
    let fixture = Fixture::new();
    let mut server = fixture.bind();
    let body = "\"\\".repeat(2048);
    assert_eq!(body.len(), 4096);
    for _ in 0..500 {
        server
            .store
            .post(
                "test-host",
                "test-host",
                &Author::human("sam"),
                &body,
                now().unwrap(),
            )
            .unwrap();
    }
    let worker = start(server, 1);
    let response = request(
        &fixture.socket(),
        &Request::Read {
            room: "test-host".into(),
            since: Some(0),
            limit: Some(500),
        },
    )
    .unwrap();
    assert!(response.ok);
    let messages = response.messages.unwrap();
    assert_eq!(messages.len(), 500);
    for (index, message) in messages.iter().enumerate() {
        assert_eq!(message.body, body);
        assert_eq!(message.seq, index as u64 + 1);
    }
    worker.join().unwrap();
}

#[test]
fn wrong_uid_refuses_hello_before_reading() {
    let fixture = Fixture::new();
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    let worker = start(fixture.bind().with_uid(uid.wrapping_add(1)), 2);
    let response = request(&fixture.socket(), &Request::Hello {}).unwrap();
    assert_eq!(
        (response.error.as_deref(), response.refused),
        (Some("refused: another user"), Some(true))
    );
    let stream = UnixStream::connect(fixture.socket()).unwrap();
    read_timeout(&stream, Duration::from_secs(1));
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"ok":false,"error":"refused: another user","refused":true})
    );
    worker.join().unwrap();
}

#[test]
fn request_framing_requires_newline_and_ignores_second_line() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 2);
    assert_eq!(
        raw(&fixture.socket(), b"{\"v\":1,\"op\":\"hello\"}")
            .error
            .as_deref(),
        Some("not a request")
    );
    assert!(raw(&fixture.socket(), b"{\"v\":1,\"op\":\"hello\"}\nnot json\n").ok);
    worker.join().unwrap();
}

#[test]
fn lock_symlink_is_not_followed() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.socket().parent().unwrap()).unwrap();
    let target = fixture.0.join("target");
    fs::write(&target, "keep").unwrap();
    symlink(&target, fixture.socket().with_extension("sock.lock")).unwrap();
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "test-host".into())
            .err()
            .as_deref(),
        Some("radio.sock could not be set up")
    );
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[test]
fn live_socket_without_our_lock_is_preserved() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.socket().parent().unwrap()).unwrap();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    assert_eq!(
        Server::bind(&fixture.socket(), &fixture.db(), "test-host".into())
            .err()
            .as_deref(),
        Some("another Radio serves radio.sock")
    );
    assert!(
        fs::symlink_metadata(fixture.socket())
            .unwrap()
            .file_type()
            .is_socket()
    );
    drop(listener);
}

#[test]
fn a_four_kib_body_can_double_when_escaped() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 3);
    fixture.create();
    let body = "\"".repeat(4096);
    assert!(wire(&post(&body)).len() > 8192);
    assert_eq!(
        request(&fixture.socket(), &post(&body)).unwrap().seq,
        Some(1)
    );
    assert_eq!(
        request(&fixture.socket(), &post(&"\"".repeat(4097)))
            .unwrap()
            .error
            .as_deref(),
        Some("the message is empty or too long")
    );
    worker.join().unwrap();
}

#[test]
fn unread_answer_hits_the_deadline_and_server_keeps_accepting() {
    let fixture = Fixture::new();
    let mut server = fixture
        .bind()
        .with_answer_deadline(Duration::from_millis(300));
    server
        .store
        .create_room("garden", "test-host", "Garden", 0.0)
        .unwrap();
    let body = "x".repeat(4096);
    for _ in 0..500 {
        server
            .store
            .post("garden", "test-host", &Author::human("sam"), &body, 0.0)
            .unwrap();
    }
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let (stream, _) = server.listener.accept().unwrap();
        let started = Instant::now();
        server.serve_one(stream);
        finished.send(started.elapsed()).unwrap();
        let (stream, _) = server.listener.accept().unwrap();
        server.serve_one(stream);
    });
    let mut client = UnixStream::connect(fixture.socket()).unwrap();
    client
        .write_all(b"{\"v\":1,\"op\":\"read\",\"room\":\"garden\",\"limit\":500}\n")
        .unwrap();
    let elapsed = completion.recv_timeout(Duration::from_secs(2));
    // Release the blocked writer even when the deadline assertion will fail.
    drop(client);
    let hello = request(&fixture.socket(), &Request::Hello {}).unwrap();
    worker.join().unwrap();
    assert!(
        elapsed.is_ok(),
        "serve_one did not return within two seconds"
    );
    assert!(elapsed.unwrap() < Duration::from_secs(2));
    assert!(hello.ok);
    assert!(hello.protocol.is_some());
}

#[test]
fn room_lists_are_paged_and_validate_limits() {
    let fixture = Fixture::new();
    let mut server = fixture.bind();
    for i in 0..500 {
        server
            .store
            .create_room(&format!("r{i:03}"), "node-a", "Room", 1.0)
            .unwrap();
    }
    let worker = start(server, 7);
    let socket = fixture.socket();
    let first = raw(&socket, b"{\"v\":1,\"op\":\"list_rooms\"}\n");
    let after = &first.rooms.as_ref().unwrap().last().unwrap().id;
    let second = raw(
        &socket,
        format!("{{\"v\":1,\"op\":\"list_rooms\",\"after\":\"{after}\"}}\n").as_bytes(),
    );
    let limited = raw(
        &socket,
        b"{\"v\":1,\"op\":\"list_rooms\",\"after\":\"r000x\",\"limit\":1}\n",
    );
    let empty = raw(
        &socket,
        b"{\"v\":1,\"op\":\"list_rooms\",\"after\":\"z\"}\n",
    );
    let mut invalid = Vec::new();
    for limit in [0, 501, u32::MAX] {
        invalid.push(raw(
            &socket,
            format!("{{\"v\":1,\"op\":\"list_rooms\",\"limit\":{limit}}}\n").as_bytes(),
        ));
    }
    worker.join().unwrap();
    let first = first.rooms.unwrap();
    assert_eq!(first.len(), 500);
    assert_eq!(first.last().unwrap().id, "r499");
    let second = second.rooms.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, "test-host");
    assert_eq!(limited.rooms.unwrap()[0].id, "r001");
    assert!(empty.rooms.unwrap().is_empty());
    for response in invalid {
        assert_eq!(response.error.as_deref(), Some("not a limit"));
    }
}

#[test]
fn largest_room_page_fits_and_round_trips_through_the_client() {
    let fixture = Fixture::new();
    let mut server = fixture.bind();
    let home = "😀".repeat(62) + "\"\\";
    let title = "😀".repeat(118) + "\"\\";
    for i in 0..500 {
        let id = format!("r{i:063}");
        server.store.create_room(&id, &home, &title, 1.0).unwrap();
    }
    let worker = start(server, 1);
    let response = request(
        &fixture.socket(),
        &Request::ListRooms {
            after: None,
            limit: None,
        },
    )
    .unwrap();
    worker.join().unwrap();
    let rooms = response.rooms.as_ref().unwrap();
    assert_eq!(rooms.len(), 500);
    assert!(serde_json::to_vec(&response).unwrap().len() + 1 < radio_client::RESPONSE_MAX_BYTES);
    for (i, room) in rooms.iter().enumerate() {
        assert_eq!(room.id, format!("r{i:063}"));
        assert_eq!(room.home, home);
        assert_eq!(room.title, title);
    }
}

#[test]
fn creating_an_existing_room_over_the_socket_names_the_conflict() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 2);
    fixture.create();
    let response = request(&fixture.socket(), &create()).unwrap();
    worker.join().unwrap();
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({"ok":false,"error":"the room already exists"})
    );
}

#[test]
fn id_creates_one_private_key_and_returns_only_its_public_id() {
    let fixture = Fixture::new();
    let link = fixture.0.join(".config/radio");
    let worker = start(fixture.bind().with_link_dir(link.clone()), 2);
    let first = raw_value(&fixture.socket(), b"{\"v\":1,\"op\":\"id\"}\n");
    let second = raw_value(&fixture.socket(), b"{\"v\":1,\"op\":\"id\"}\n");
    worker.join().unwrap();
    assert_eq!(first, second);
    assert_eq!(first["ok"], true);
    let id = first["id"].as_str().unwrap();
    assert_eq!(id.len(), 64);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        fs::metadata(&link).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let path = link.join("node.key");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len(), 32);
    let secret_hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let line = serde_json::to_string(&first).unwrap();
    assert!(!line.contains(&secret_hex));
    let hex_runs: Vec<_> = line
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|part| part.len() == 64)
        .collect();
    assert_eq!(hex_runs, [id]);
    assert_eq!(first.as_object().unwrap().len(), 2);
}

#[test]
fn id_refuses_a_loose_key_without_stopping_rooms_or_repairing_it() {
    let fixture = Fixture::new();
    let link = fixture.0.join(".config/radio");
    fs::create_dir_all(&link).unwrap();
    let key = link.join("node.key");
    let bytes = [7; 32];
    fs::write(&key, bytes).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    let worker = start(fixture.bind().with_link_dir(link), 4);
    fixture.create();
    let answer = raw_value(&fixture.socket(), b"{\"v\":1,\"op\":\"id\"}\n");
    let posted = request(&fixture.socket(), &post("hello")).unwrap();
    let read = request(
        &fixture.socket(),
        &Request::Read {
            room: "garden".into(),
            since: None,
            limit: None,
        },
    )
    .unwrap();
    worker.join().unwrap();
    assert_eq!(
        answer,
        serde_json::json!({"ok":false,"error":"the link key is open to others"})
    );
    assert!(fs::read(&key).unwrap() == bytes);
    assert_eq!(
        fs::metadata(key).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(posted.ok);
    assert!(read.ok);
    assert_eq!(read.messages.unwrap()[0].body, "hello");
}

#[test]
fn id_without_a_link_folder_reports_not_set_up() {
    let fixture = Fixture::new();
    let worker = start(fixture.bind(), 1);
    let answer = raw_value(&fixture.socket(), b"{\"v\":1,\"op\":\"id\"}\n");
    worker.join().unwrap();
    assert_eq!(
        answer,
        serde_json::json!({"ok":false,"error":"the link is not set up"})
    );
}

fn logged_request(line: &[u8], wrong_uid: bool) -> String {
    let fixture = Fixture::new();
    let mut server = fixture.bind();
    let path = fixture.0.join("events");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    server = server.with_log(crate::log::Log::to_file(file, crate::log::LOG_MAX_BYTES));
    if wrong_uid {
        let uid = server.own_uid.wrapping_add(1);
        server = server.with_uid(uid);
    }
    let worker = start(server, 1);
    let response = raw(&fixture.socket(), line);
    assert!(!response.ok);
    worker.join().unwrap();
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .map(|line| line.split_once(' ').unwrap().1)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn log_refused_post_omits_client_text() {
    let text = logged_request(br#"{"v":1,"op":"post","room":"test-host","author":{"kind":"kiteapp-agent","name":"wren"},"body":"quiet garden words"}
"#, false);
    assert_eq!(text, "serve refused op=\"post\" error=\"not a member\"");
    assert!(!text.contains("quiet garden words"));
    assert!(!text.contains("wren"));
}
#[test]
fn log_missing_room() {
    assert_eq!(
        logged_request(b"{\"v\":1,\"op\":\"read\",\"room\":\"missing\"}\n", false),
        "serve failed op=\"read\" error=\"no such room\""
    );
}
#[test]
fn log_invalid_json() {
    assert_eq!(
        logged_request(b"not json\n", false),
        "serve failed op=\"unknown\" error=\"not a request\""
    );
}
#[test]
fn log_unknown_op() {
    assert_eq!(
        logged_request(b"{\"v\":1,\"op\":\"quiet garden words\\nwren\"}\n", false),
        "serve failed op=\"unknown\" error=\"not a request\""
    );
}
#[test]
fn log_missing_op() {
    assert_eq!(
        logged_request(b"{\"v\":1}\n", false),
        "serve failed op=\"unknown\" error=\"not a request\""
    );
}
#[test]
fn log_wrong_uid() {
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    assert_eq!(
        logged_request(b"", true),
        format!("serve refused op=\"unknown\" error=\"refused: another user\" uid={uid}")
    );
}
#[test]
fn log_unsupported_version() {
    assert_eq!(
        logged_request(b"{\"v\":2,\"op\":\"hello\"}\n", false),
        "serve failed op=\"hello\" error=\"unsupported_version\""
    );
}
#[test]
fn log_successful_hello_and_post_are_quiet() {
    let fixture = Fixture::new();
    let server = fixture.bind();
    let path = fixture.0.join("events");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let worker = start(server.with_log(crate::log::Log::to_file(file, 1024)), 2);
    assert!(request(&fixture.socket(), &Request::Hello {}).unwrap().ok);
    let mut message = post("quiet garden words");
    if let Request::Post { room, .. } = &mut message {
        *room = "test-host".into();
    }
    assert!(request(&fixture.socket(), &message).unwrap().ok);
    worker.join().unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "");
}

#[test]
fn log_unanswered_hello_is_the_only_event() {
    let fixture = Fixture::new();
    let server = fixture.bind().with_answer_deadline(Duration::ZERO);
    let path = fixture.0.join("events");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let worker = start(
        server.with_log(Log::to_file(file, crate::log::LOG_MAX_BYTES)),
        1,
    );
    let mut client = UnixStream::connect(fixture.socket()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client.write_all(b"{\"v\":1,\"op\":\"hello\"}\n").unwrap();
    let mut answer = String::new();
    BufReader::new(client).read_to_string(&mut answer).unwrap();
    worker.join().unwrap();
    assert!(answer.is_empty());
    let text = fs::read_to_string(path).unwrap();
    let events: Vec<_> = text
        .lines()
        .map(|line| line.split_once(' ').unwrap().1)
        .collect();
    assert_eq!(events, ["serve unanswered op=\"hello\""]);
}

#[test]
fn fleet_and_join_codes_cross_the_socket_without_logging_secrets() {
    let fixture = Fixture::new();
    let link = fixture.0.join(".config/radio");
    let log_path = fixture.0.join("radio.log");
    let server = fixture.bind();
    let log = Log::to_file(fs::File::create(&log_path).unwrap(), u64::MAX);
    let worker = start(
        server
            .with_link_dir(link)
            .with_fleet_db(fixture.0.join("fleet.db"))
            .with_log(log),
        11,
    );
    let send = |value: serde_json::Value| {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        raw_value(&fixture.socket(), &line)
    };
    assert_eq!(
        send(serde_json::json!({"v":1,"op":"code_create"})),
        serde_json::json!({"ok":false,"error":"not in a fleet"})
    );
    let id = send(serde_json::json!({"v":1,"op":"id"}))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let found = send(serde_json::json!({"v":1,"op":"fleet_init","name":"garden"}));
    assert_eq!(found["ok"], true);
    assert_eq!(found["fleet"]["founder"], id);
    assert_eq!(found["fleet"]["name"], "garden");
    assert_eq!(found["fleet"]["version"], 1);
    let recovery = PathBuf::from(found["recovery_file"].as_str().unwrap());
    assert!(recovery.is_absolute());
    assert_eq!(
        fs::metadata(&recovery).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let recovery = fs::read(&recovery).unwrap();
    assert_eq!(recovery.len(), 65);
    assert_eq!(recovery[64], b'\n');
    assert!(
        recovery[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    );
    let recovery_secret = std::str::from_utf8(&recovery[..64]).unwrap();
    assert!(!found.to_string().contains(recovery_secret));
    assert_eq!(
        send(serde_json::json!({"v":1,"op":"fleet_init","name":"orchard"})),
        serde_json::json!({"ok":false,"error":"already in a fleet"})
    );
    let before = now().unwrap();
    let created = send(serde_json::json!({"v":1,"op":"code_create","name":"orchard"}));
    let after = now().unwrap();
    assert_eq!(created["ok"], true);
    let text = created["code"].as_str().unwrap();
    let code = radio_link::code::JoinCode::parse(text).unwrap();
    assert_eq!(code.fleet, found["fleet"]["id"].as_str().unwrap());
    assert_eq!(code.inviter.to_string(), id);
    assert!(code.hints.direct.is_empty());
    let expires = created["expires_at"].as_f64().unwrap();
    assert!((before + 1800.0..=after + 1800.0).contains(&expires));
    let listed = send(serde_json::json!({"v":1,"op":"code_list"}));
    let rows = listed["codes"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], created["code_id"]);
    assert_eq!(rows[0]["name"], "orchard");
    assert_eq!(rows[0]["uses"], 1);
    let mut fields: Vec<_> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(fields, ["created", "expires_at", "id", "name", "uses"]);
    let secret = text.split('.').nth(3).unwrap();
    assert!(!listed.to_string().contains(text));
    assert!(!listed.to_string().contains(secret));
    for removed in [true, false] {
        assert_eq!(
            send(serde_json::json!({"v":1,"op":"code_revoke","id":created["code_id"]})),
            serde_json::json!({"ok":true,"removed":removed})
        );
    }
    assert_eq!(
        send(serde_json::json!({"v":1,"op":"code_list"})),
        serde_json::json!({"ok":true,"codes":[]})
    );
    assert_eq!(
        send(serde_json::json!({"v":1,"op":"code_create","ttl":59})),
        serde_json::json!({"ok":false,"error":"not a code lifetime"})
    );
    // A hostile failure must not carry its supplied secret into the log either.
    assert_eq!(
        send(serde_json::json!({"v":1,"op":"code_revoke","id":text})),
        serde_json::json!({"ok":false,"error":"not a code id"})
    );
    worker.join().unwrap();
    let log = fs::read_to_string(log_path).unwrap();
    assert!(log.contains("not a code lifetime"));
    assert!(!log.contains(recovery_secret));
    assert!(!log.contains(secret));
    assert!(!log.contains(text));
}

#[test]
fn fleet_ops_need_both_link_and_fleet_paths() {
    for configured in [false, true] {
        let fixture = Fixture::new();
        let server = fixture.bind();
        let server = if configured {
            server.with_link_dir(fixture.0.join("link"))
        } else {
            server.with_fleet_db(fixture.0.join("fleet.db"))
        };
        let worker = start(server, 8);
        for value in [
            serde_json::json!({"v":1,"op":"fleet_init","name":"garden"}),
            serde_json::json!({"v":1,"op":"code_create"}),
            serde_json::json!({"v":1,"op":"code_list"}),
            serde_json::json!({"v":1,"op":"code_revoke","id":"0000000000000000"}),
            serde_json::json!({"v":1,"op":"computer_list"}),
            serde_json::json!({"v":1,"op":"computer_accept","computer":"meadow"}),
            serde_json::json!({"v":1,"op":"computer_block","computer":"meadow"}),
            serde_json::json!({"v":1,"op":"computer_trust","computer":"meadow"}),
        ] {
            let mut line = serde_json::to_vec(&value).unwrap();
            line.push(b'\n');
            assert_eq!(
                raw_value(&fixture.socket(), &line),
                serde_json::json!({"ok":false,"error":"the link is not set up"})
            );
        }
        worker.join().unwrap();
    }
}

#[test]
fn recovery_folder_is_absolute_even_with_a_relative_home() {
    let absolute = absolute_link_dir(Path::new(".")).unwrap();
    assert!(absolute.is_absolute());
    assert_eq!(
        absolute,
        std::env::current_dir().unwrap().canonicalize().unwrap()
    );
    let fixture = Fixture::new();
    assert_eq!(
        absolute_link_dir(&fixture.0),
        Err("the link folder is not usable")
    );
}

#[test]
fn computers_cross_the_socket() {
    use radio_link::{
        SecretKey,
        fleet_db::FleetDb,
        membership::{self, RecordBody},
    };

    let fixture = Fixture::new();
    let link = fixture.0.join(".config/radio");
    let fleet_path = fixture.0.join("fleet.db");
    let log_path = fixture.0.join("radio.log");
    let server = fixture
        .bind()
        .with_link_dir(link.clone())
        .with_fleet_db(fleet_path.clone())
        .with_log(Log::to_file(fs::File::create(&log_path).unwrap(), u64::MAX));
    let worker = start(server, 10);
    let send = |request: &Request| {
        let mut value = serde_json::to_value(request).unwrap();
        value["v"] = 1.into();
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        raw_value(&fixture.socket(), &line)
    };
    assert_eq!(
        send(&Request::ComputerList {}),
        serde_json::json!({"ok":false,"error":"not in a fleet"})
    );
    let found = send(&Request::FleetInit {
        name: "garden".into(),
    });
    assert_eq!(found["ok"], true);
    let created = send(&Request::CodeCreate {
        name: None,
        ttl: None,
    });
    assert_eq!(created["ok"], true);
    let code = radio_link::code::JoinCode::parse(created["code"].as_str().unwrap()).unwrap();
    let node = radio_link::key::load_or_create(&link).unwrap();
    let meadow = SecretKey::from_bytes(&[21; 32]);
    let lantern = SecretKey::from_bytes(&[22; 32]);
    let mut db = FleetDb::open(&fleet_path).unwrap();
    db.admit(
        &node,
        "test-host",
        &[],
        &code,
        &meadow.public(),
        "meadow",
        &["127.0.0.1:7000".parse().unwrap()],
        now().unwrap(),
    )
    .unwrap();
    let introduction = membership::sign(
        RecordBody::Introduction {
            fleet: found["fleet"]["id"].as_str().unwrap().into(),
            inviter: meadow.public().to_string(),
            counter: 1,
            newcomer: lantern.public().to_string(),
            name: "lantern".into(),
            addrs: vec!["127.0.0.1:7001".parse().unwrap()],
        },
        &meadow,
    );
    db.apply(&node.public(), &introduction, now().unwrap())
        .unwrap();
    let mut lantern_row = serde_json::json!({
        "id":lantern.public().to_string(), "name":"lantern", "state":"pending",
        "invited_by":meadow.public().to_string(), "trusted":false
    });
    let mut meadow_row = serde_json::json!({
        "id":meadow.public().to_string(), "name":"meadow", "state":"accepted",
        "invited_by":node.public().to_string(), "trusted":false
    });
    assert_eq!(
        send(&Request::ComputerList {}),
        serde_json::json!({"ok":true,"computers":[lantern_row,meadow_row]})
    );
    assert_eq!(
        send(&Request::ComputerTrust {
            computer: "lantern".into()
        }),
        serde_json::json!({"ok":false,"error":"only an accepted computer can be trusted"})
    );
    lantern_row["state"] = "accepted".into();
    assert_eq!(
        send(&Request::ComputerAccept {
            computer: "lantern".into()
        }),
        serde_json::json!({"ok":true,"computer":lantern_row})
    );
    lantern_row["state"] = "blocked".into();
    assert_eq!(
        send(&Request::ComputerBlock {
            computer: "lantern".into()
        }),
        serde_json::json!({"ok":true,"computer":lantern_row})
    );
    lantern_row["state"] = "accepted".into();
    assert_eq!(
        send(&Request::ComputerAccept {
            computer: lantern.public().to_string()
        }),
        serde_json::json!({"ok":true,"computer":lantern_row})
    );
    meadow_row["trusted"] = true.into();
    assert_eq!(
        send(&Request::ComputerTrust {
            computer: "meadow".into()
        }),
        serde_json::json!({"ok":true,"computer":meadow_row})
    );
    // Invalid input must not leak its supplied name into a failure log either.
    assert_eq!(
        send(&Request::ComputerBlock {
            computer: "Meadow".into()
        }),
        serde_json::json!({"ok":false,"error":"not a computer"})
    );
    worker.join().unwrap();
    let log = fs::read_to_string(log_path).unwrap();
    assert!(log.contains("only an accepted computer can be trusted"));
    assert!(log.contains("not a computer"));
    assert!(!log.to_ascii_lowercase().contains("meadow"));
    assert!(!log.contains("lantern"));
}
