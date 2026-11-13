use radio_client::{
    Member, Message, OPS, PROTOCOL_MAJOR, PROTOCOL_MINOR, Protocol, REQUEST_MAX_BYTES, Request,
    Response, Room, StatusLine,
};
use radio_rooms::{Author, OWNED, READ_MAX, RoomStore};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The protocol majors this daemon serves.
pub const SUPPORTED_MAJORS: &[u32] = &[1];
const TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const NOT_REQUEST: &str = "not a request";
const TOO_LONG: &str = "the request is too long";
const TOOK_TOO_LONG: &str = "the request took too long";
const REFUSED: &str = "refused: another user";
const UNBOUND: &str = "radio.sock could not be set up";
const NOT_MEMBER: &str = "not a member";

fn success() -> Response {
    Response {
        ok: true,
        ..Default::default()
    }
}
fn failure(error: String) -> Response {
    Response {
        refused: [REFUSED, NOT_MEMBER, OWNED]
            .contains(&error.as_str())
            .then_some(true),
        error: Some(error),
        ..Default::default()
    }
}

/// A serial local socket server that owns the rooms store and socket lock.
pub struct Server {
    listener: UnixListener,
    /// Holds the exclusive socket lock until the server is dropped.
    _lock: fs::File,
    store: RoomStore,
    host: String,
    link_dir: Option<PathBuf>,
    own_uid: u32,
    request_deadline: Duration,
    answer_deadline: Duration,
}

impl Server {
    /// Binds `socket` and opens the store, making the home room `<host>` when it is missing.
    pub fn bind(socket: &Path, rooms_db: &Path, host: String) -> Result<Self, String> {
        let parent = socket.parent().ok_or(UNBOUND)?;
        radio_rooms::private_dir(parent, true).map_err(|_| UNBOUND)?;
        let mut lock_path = socket.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)
            .map_err(|_| UNBOUND)?;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| UNBOUND)?;
        // SAFETY: the file descriptor is live and flock has no memory preconditions.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN) {
                return Err("another Radio serves radio.sock".into());
            }
            return Err(UNBOUND.into());
        }
        match fs::symlink_metadata(socket) {
            Ok(meta) if meta.file_type().is_socket() => {
                if UnixStream::connect(socket).is_ok() {
                    return Err("another Radio serves radio.sock".into());
                }
                fs::remove_file(socket).map_err(|_| UNBOUND)?;
            }
            Ok(_) => return Err("radio.sock is not a socket".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(UNBOUND.into()),
        }
        let listener = UnixListener::bind(socket).map_err(|_| UNBOUND)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).map_err(|_| UNBOUND)?;
        let mut store = RoomStore::open(rooms_db)?;
        store.ensure_room(&host, &host, &format!("Room on {host}"), now()?)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        let own_uid = unsafe { libc::geteuid() };
        Ok(Self {
            listener,
            _lock: lock,
            store,
            host,
            link_dir: None,
            own_uid,
            request_deadline: TIMEOUT,
            answer_deadline: TIMEOUT,
        })
    }

    /// Sets the folder holding this computer's link key.
    pub fn with_link_dir(mut self, dir: PathBuf) -> Self {
        self.link_dir = Some(dir);
        self
    }

    fn serve_one(&mut self, mut stream: UnixStream) {
        let accepted = Instant::now();
        if timeouts(&stream).is_err() {
            return;
        }
        let response = if peer_uid(&stream) != Some(self.own_uid) {
            failure(REFUSED.into())
        } else {
            let result = read_request(&stream, accepted, self.request_deadline)
                .and_then(|line| serde_json::from_slice(&line).map_err(|_| NOT_REQUEST.to_owned()))
                .and_then(|value| self.dispatch(value));
            result.unwrap_or_else(failure)
        };
        let _ = write_line(&mut stream, &response, self.answer_deadline);
    }

    fn dispatch(&mut self, mut value: serde_json::Value) -> Result<Response, String> {
        let object = value.as_object_mut().ok_or(NOT_REQUEST)?;
        let version = object
            .get("v")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(NOT_REQUEST)?;
        if !SUPPORTED_MAJORS.contains(&version) {
            return Ok(Response {
                error: Some("unsupported_version".into()),
                supported: Some(SUPPORTED_MAJORS.to_vec()),
                ..Default::default()
            });
        }
        object.remove("v");
        let request = serde_json::from_value(value).map_err(|_| NOT_REQUEST)?;
        self.apply(request)
    }

    fn apply(&mut self, request: Request) -> Result<Response, String> {
        let now = now()?;
        let mut response = success();
        match request {
            Request::Hello {} => {
                response.radio = Some(env!("CARGO_PKG_VERSION").into());
                response.protocol = Some(Protocol {
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR,
                });
                response.majors = Some(SUPPORTED_MAJORS.to_vec());
                response.ops = Some(OPS.iter().map(|op| (*op).into()).collect());
            }
            Request::Id {} => {
                let dir = self.link_dir.as_deref().ok_or("the link is not set up")?;
                let key = radio_link::key::load_or_create(dir)?;
                response.id = Some(radio_link::key::endpoint_id(&key));
            }
            Request::Register {
                app,
                app_version,
                members,
            } => {
                let members: Vec<_> = members.into_iter().map(author).collect();
                self.store.register(&app, &app_version, &members, now)?;
            }
            Request::ListRooms { after, limit } => {
                if limit.is_some_and(|limit| limit == 0 || limit as usize > READ_MAX) {
                    return Err("not a limit".into());
                }
                response.rooms = Some(
                    self.store
                        .rooms(after.as_deref(), limit.unwrap_or(500) as usize)?
                        .into_iter()
                        .map(|row| Room {
                            id: row.id,
                            home: row.home,
                            title: row.title,
                            created: row.created,
                            last_seq: row.last_seq,
                        })
                        .collect(),
                );
            }
            Request::Read { room, since, limit } => {
                self.require_room(&room)?;
                if limit.is_some_and(|limit| limit == 0 || limit as usize > READ_MAX) {
                    return Err("not a limit".into());
                }
                let messages = match since {
                    Some(since) => self
                        .store
                        .since(&room, since, limit.unwrap_or(500) as usize)?,
                    None => self.store.tail(&room, limit.unwrap_or(50) as usize)?,
                };
                response.messages = Some(
                    messages
                        .into_iter()
                        .map(|row| Message {
                            seq: row.seq,
                            kind: row.kind.word().into(),
                            author_host: row.author_host,
                            author: member(row.author),
                            body: row.body,
                            sent_at: row.sent_at,
                            delivered_at: row.delivered_at,
                        })
                        .collect(),
                );
            }
            Request::Status { room } => {
                self.require_room(&room)?;
                response.status = Some(
                    self.store
                        .status(&room, now)?
                        .into_iter()
                        .map(|row| StatusLine {
                            key: row.key,
                            line: row.line,
                            set_by: member(row.set_by),
                            set_at: row.set_at,
                            expires_at: row.expires_at,
                        })
                        .collect(),
                );
            }
            Request::CreateRoom { room, title } => {
                self.store.create_room(&room, &self.host, &title, now)?;
            }
            Request::Post { room, author, body } => {
                let author = self.member(author)?;
                let seq = self.store.post(&room, &self.host, &author, &body, now)?;
                response.seq = Some(seq);
            }
            Request::StatusSet {
                room,
                key,
                line,
                author,
                expires_at,
            } => {
                let author = self.member(author)?;
                let seq = self
                    .store
                    .set_status(&room, &key, &line, &self.host, &author, expires_at, now)?;
                response.seq = Some(seq);
            }
            Request::StatusClear { room, key, author } => {
                let author = self.member(author)?;
                let seq = self
                    .store
                    .clear_status(&room, &key, &self.host, &author, now)?;
                response.removed = Some(seq.is_some());
                response.seq = seq;
            }
        }
        Ok(response)
    }

    fn require_room(&self, room: &str) -> Result<(), String> {
        self.store
            .room(room)?
            .ok_or_else(|| "no such room".to_owned())
            .map(|_| ())
    }

    fn member(&self, member: Member) -> Result<Author, String> {
        let author = author(member);
        if self.store.is_member(&author)? {
            Ok(author)
        } else {
            Err(NOT_MEMBER.into())
        }
    }

    /// Accepts and answers connections one at a time until the process stops.
    pub fn serve(mut self) {
        loop {
            let incoming = self.listener.incoming().next();
            match incoming {
                Some(Ok(stream)) => self.serve_one(stream),
                Some(Err(_)) => std::thread::sleep(ACCEPT_BACKOFF),
                None => {}
            }
        }
    }

    /// Overrides the allowed peer user identifier for tests.
    #[cfg(test)]
    pub(crate) fn with_uid(mut self, uid: u32) -> Self {
        self.own_uid = uid;
        self
    }

    /// Overrides the whole-request time budget for tests.
    #[cfg(test)]
    pub(crate) fn with_deadline(mut self, deadline: Duration) -> Self {
        self.request_deadline = deadline;
        self
    }

    /// Overrides the whole-answer write time budget for tests.
    #[cfg(test)]
    pub(crate) fn with_answer_deadline(mut self, deadline: Duration) -> Self {
        self.answer_deadline = deadline;
        self
    }
}

fn now() -> Result<f64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .map_err(|_| "not a time".to_owned())
}

fn timeouts(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))
}

fn read_request(
    mut stream: &UnixStream,
    accepted: Instant,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let remaining = deadline
            .checked_sub(accepted.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or(TOOK_TOO_LONG)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| NOT_REQUEST)?;
        let limit = buffer.len().min(REQUEST_MAX_BYTES + 1 - line.len());
        let count = match stream.read(&mut buffer[..limit]) {
            Ok(0) => return Err(NOT_REQUEST.into()),
            Ok(count) => count,
            Err(error) => match error.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    return Err(TOOK_TOO_LONG.into());
                }
                std::io::ErrorKind::Interrupted => continue,
                _ => return Err(NOT_REQUEST.into()),
            },
        };
        if accepted.elapsed() >= deadline {
            return Err(TOOK_TOO_LONG.into());
        }
        let bytes = &buffer[..count];
        let end = bytes.iter().position(|byte| *byte == b'\n');
        line.extend_from_slice(&bytes[..end.map_or(count, |index| index + 1)]);
        if line.len() > REQUEST_MAX_BYTES {
            return Err(TOO_LONG.into());
        }
        if end.is_some() {
            return Ok(line);
        }
    }
}

fn write_line(
    stream: &mut UnixStream,
    value: &Response,
    deadline: Duration,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let ready = Instant::now();
    let mut remaining_bytes = bytes.as_slice();
    while !remaining_bytes.is_empty() {
        let remaining = deadline
            .checked_sub(ready.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(remaining_bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => remaining_bytes = &remaining_bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: the fd is live and both output pointers refer to initialized uid/gid storage.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (result == 0).then_some(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the fd is live; the output buffer and length pointer have the advertised size.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (result == 0 && len as usize == std::mem::size_of::<libc::ucred>()).then_some(credentials.uid)
}

fn author(member: Member) -> Author {
    Author::member(member.kind, member.name)
}
fn member(author: Author) -> Member {
    Member {
        kind: author.kind,
        name: author.name,
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "contract_tests.rs"]
mod contract_tests;
