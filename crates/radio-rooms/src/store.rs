use rusqlite::{Connection, OpenFlags, Row, TransactionBehavior, params};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// The phrase for an empty or over-long body.
pub const MESSAGE_INVALID: &str = "the message is empty or too long";
/// The maximum message body size, in bytes.
pub const BODY_MAX_BYTES: usize = 4096;
/// The maximum status line length, in characters.
pub const STATUS_LINE_MAX_CHARS: usize = 200;
/// The maximum number of live status keys per room.
pub const STATUS_MAX_KEYS: usize = 20;
/// The maximum combined live status text size per room, in bytes.
pub const STATUS_MAX_BYTES: usize = 2048;
/// The maximum status key length, in characters.
pub const KEY_MAX_CHARS: usize = 40;
/// The maximum cleaned name length, in characters.
pub const NAME_MAX_CHARS: usize = 64;
/// The default filename for the rooms store.
pub const DB_FILE: &str = "rooms.db";
const STORE_OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const NEWER: &str = "the room store is newer than this build";
const UNREADABLE: &str = "the room store could not be read";
const UNWRITABLE: &str = "the room store could not be written";
/// The refusal returned when a nonhuman author changes another member's status.
pub const OWNED: &str = "the status line belongs to someone else";

const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE schema_version (version INTEGER NOT NULL);
    CREATE TABLE rooms (
        id TEXT PRIMARY KEY, home TEXT NOT NULL, title TEXT NOT NULL, created REAL NOT NULL
    ) STRICT;
    CREATE TABLE messages (
        room TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL, author_host TEXT NOT NULL,
        author_kind TEXT NOT NULL CHECK (@author_kind@),
        author TEXT NOT NULL, body TEXT NOT NULL, sent_at REAL NOT NULL, delivered_at REAL,
        PRIMARY KEY (room, seq)
    ) STRICT, WITHOUT ROWID;
    CREATE TABLE room_status (
        room TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        key TEXT NOT NULL, line TEXT NOT NULL, set_by TEXT NOT NULL,
        set_at REAL NOT NULL, expires_at REAL, PRIMARY KEY (room, key)
    ) STRICT, WITHOUT ROWID;
    CREATE TABLE outbox (
        id INTEGER PRIMARY KEY, room TEXT NOT NULL, home TEXT NOT NULL,
        author_kind TEXT NOT NULL CHECK (@author_kind@),
        author TEXT NOT NULL, body TEXT NOT NULL, queued_at REAL NOT NULL
    ) STRICT;
",
    "
    ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'post'
        CHECK (kind IN ('post','status_set','status_clear'));
    ALTER TABLE room_status ADD COLUMN set_by_kind TEXT NOT NULL DEFAULT 'human'
        CHECK (@set_by_kind@);
",
    "
    CREATE TABLE messages_new (
        room TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL, author_host TEXT NOT NULL,
        author_kind TEXT NOT NULL CHECK (@author_kind@),
        author TEXT NOT NULL, body TEXT NOT NULL, sent_at REAL NOT NULL, delivered_at REAL,
        kind TEXT NOT NULL DEFAULT 'post' CHECK (kind IN ('post','status_set','status_clear')),
        PRIMARY KEY (room, seq)
    ) STRICT, WITHOUT ROWID;
    INSERT INTO messages_new (room, seq, author_host, author_kind, author, body, sent_at, delivered_at, kind)
        SELECT room, seq, author_host, author_kind, author, body, sent_at, delivered_at, kind FROM messages;
    DROP TABLE messages;
    ALTER TABLE messages_new RENAME TO messages;
    CREATE TABLE room_status_new (
        room TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        key TEXT NOT NULL, line TEXT NOT NULL, set_by TEXT NOT NULL,
        set_at REAL NOT NULL, expires_at REAL,
        set_by_kind TEXT NOT NULL CHECK (@set_by_kind@),
        PRIMARY KEY (room, key)
    ) STRICT, WITHOUT ROWID;
    INSERT INTO room_status_new (room, key, line, set_by, set_at, expires_at, set_by_kind)
        SELECT room, key, line, set_by, set_at, expires_at, set_by_kind FROM room_status;
    DROP TABLE room_status;
    ALTER TABLE room_status_new RENAME TO room_status;
    CREATE TABLE outbox_new (
        id INTEGER PRIMARY KEY, room TEXT NOT NULL, home TEXT NOT NULL,
        author_kind TEXT NOT NULL CHECK (@author_kind@),
        author TEXT NOT NULL, body TEXT NOT NULL, queued_at REAL NOT NULL
    ) STRICT;
    INSERT INTO outbox_new (id, room, home, author_kind, author, body, queued_at)
        SELECT id, room, home, author_kind, author, body, queued_at FROM outbox;
    DROP TABLE outbox;
    ALTER TABLE outbox_new RENAME TO outbox;
    CREATE TABLE apps (app TEXT PRIMARY KEY, app_version TEXT NOT NULL, registered_at REAL NOT NULL) STRICT;
    CREATE TABLE members (
        app TEXT NOT NULL REFERENCES apps(app) ON DELETE CASCADE,
        kind TEXT NOT NULL CHECK (@kind@ AND kind <> 'human'),
        name TEXT NOT NULL,
        PRIMARY KEY (app, kind, name)
    ) STRICT, WITHOUT ROWID;
",
    "
    CREATE TABLE imports (
        source TEXT PRIMARY KEY, imported_at REAL NOT NULL,
        rooms INTEGER NOT NULL, messages INTEGER NOT NULL, statuses INTEGER NOT NULL, skipped INTEGER NOT NULL
    ) STRICT;
",
];

/// The reserved kind for the person at the computer.
pub const HUMAN: &str = "human";
/// The maximum number of messages returned by one read.
pub const READ_MAX: usize = 500;
/// The maximum number of members accepted in one registration.
pub const MEMBERS_MAX: usize = 256;

/// A message author or status owner, identified by kind and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The member kind, with `human` reserved for the person at the computer.
    pub kind: String,
    /// The member name within its kind.
    pub name: String,
}
impl Author {
    /// Creates a human author without cleaning or validating the name.
    pub fn human(name: impl Into<String>) -> Self {
        Self::member(HUMAN, name)
    }
    /// Creates an author without cleaning or validating its kind or name.
    pub fn member(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
        }
    }
    /// Returns whether this author has the reserved human kind.
    pub fn is_human(&self) -> bool {
        self.kind == HUMAN
    }
    fn parts(&self) -> (&str, &str) {
        (&self.kind, &self.name)
    }
}
/// Checks for a valid kind: 1 to 40 lowercase ASCII letters, digits, underscores or hyphens, starting with a letter.
pub fn is_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 40
        && kind.as_bytes()[0].is_ascii_lowercase()
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// The stored kinds of room messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// A posted message.
    Post,
    /// A logged status update.
    StatusSet,
    /// A logged status removal.
    StatusClear,
}

impl MessageKind {
    /// Returns the word stored for this message kind.
    pub fn word(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::StatusSet => "status_set",
            Self::StatusClear => "status_clear",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [Self::Post, Self::StatusSet, Self::StatusClear]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// A stored room message, including status changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// The message sequence number within its room.
    pub seq: u64,
    /// Whether this is a post, a status update or a status removal.
    pub kind: MessageKind,
    /// The host that wrote the message.
    pub author_host: String,
    /// The member who wrote the message.
    pub author: Author,
    /// The message text.
    pub body: String,
    /// When the message was sent, in seconds since the Unix epoch.
    pub sent_at: f64,
    /// When delivery was recorded, in seconds since the Unix epoch, if known.
    pub delivered_at: Option<f64>,
}

/// A room and its latest message sequence number.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomRow {
    /// The room identifier.
    pub id: String,
    /// The host where the room is homed.
    pub home: String,
    /// The room title.
    pub title: String,
    /// When the room was created, in seconds since the Unix epoch.
    pub created: f64,
    /// The latest message sequence number, or zero for an empty room.
    pub last_seq: u64,
}

/// A room status line and its owner and expiry.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusLine {
    /// The status key within its room.
    pub key: String,
    /// The status text.
    pub line: String,
    /// The member who last set this status line.
    pub set_by: Author,
    /// When the status was set, in seconds since the Unix epoch.
    pub set_at: f64,
    /// When the status expires, in seconds since the Unix epoch, if set.
    pub expires_at: Option<f64>,
}

#[path = "import.rs"]
mod import;
pub use import::{Import, ImportReport};

#[path = "snapshot.rs"]
mod snapshot;
pub use snapshot::{restore_store, snapshot_store};

/// A private SQLite rooms store with one writing daemon.
pub struct RoomStore {
    conn: Connection,
    path: PathBuf,
}

impl RoomStore {
    /// Opens or creates a private store and applies missing schema migrations.
    pub fn open(path: &Path) -> Result<Self, String> {
        let parent = path.parent().ok_or("the rooms directory is missing")?;
        crate::private_dir(parent, true)
            .map_err(|reason| format!("the rooms directory {reason}"))?;
        let path = parent
            .canonicalize()
            .map_err(|_| "the room store could not be opened")?
            .join(
                path.file_name()
                    .ok_or("the room store could not be opened")?,
            );
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| "the room store could not be opened")?;
        if !file.metadata().map_err(|_| UNREADABLE)?.is_file() {
            return Err("the room store is not a file".into());
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "the room store could not be opened")?;
        let mut conn = Connection::open_with_flags(&path, STORE_OPEN_FLAGS)
            .map_err(|_| "the room store could not be opened")?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|_| UNWRITABLE)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| UNWRITABLE)?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(|_| UNWRITABLE)?;
        migrate(&mut conn)?;
        Ok(Self { conn, path })
    }

    /// Creates a room; an existing identifier is an error.
    pub fn create_room(
        &mut self,
        id: &str,
        home: &str,
        title: &str,
        now: f64,
    ) -> Result<(), String> {
        self.insert_room(
            "INSERT INTO rooms (id, home, title, created) VALUES (?1, ?2, ?3, ?4)",
            id,
            home,
            title,
            now,
        )
        .map(|_| ())
    }

    /// Creates a missing room and returns whether it was inserted, leaving an existing room unchanged.
    pub fn ensure_room(
        &mut self,
        id: &str,
        home: &str,
        title: &str,
        now: f64,
    ) -> Result<bool, String> {
        self.insert_room(
            "INSERT INTO rooms (id, home, title, created) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING",
            id,
            home,
            title,
            now,
        )
    }

    fn insert_room(
        &mut self,
        sql: &str,
        id: &str,
        home: &str,
        title: &str,
        now: f64,
    ) -> Result<bool, String> {
        let (home, title, now) = clean_room(id, home, title, now)?;
        self.conn
            .prepare_cached(sql)
            .and_then(|mut stmt| stmt.execute(params![id, home, title, now]))
            .map(|inserted| inserted == 1)
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
                {
                    "the room already exists".to_owned()
                }
                _ => UNWRITABLE.to_owned(),
            })
    }

    /// Appends a cleaned message and returns its sequence number.
    pub fn post(
        &mut self,
        room: &str,
        author_host: &str,
        author: &Author,
        body: &str,
        now: f64,
    ) -> Result<u64, String> {
        let (author_host, author, body, now) = clean_message(author_host, author, body, now)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        require_room(&tx, room)?;
        let seq = append(
            &tx,
            room,
            &author_host,
            &author,
            MessageKind::Post,
            &body,
            now,
            None,
        )?;
        tx.commit().map_err(|_| UNWRITABLE)?;
        Ok(seq)
    }

    /// Sets and logs a status line; `OWNED` refuses changes to another nonhuman member's line.
    #[allow(clippy::too_many_arguments)]
    pub fn set_status(
        &mut self,
        room: &str,
        key: &str,
        line: &str,
        author_host: &str,
        by: &Author,
        expires_at: Option<f64>,
        now: f64,
    ) -> Result<u64, String> {
        if !is_key(key, KEY_MAX_CHARS) {
            return Err("not a status key".into());
        }
        let author_host = name(author_host)?;
        let StatusLine {
            line,
            set_by: by,
            set_at: now,
            expires_at,
            ..
        } = clean_status(key, line, by, now, expires_at)?;
        if expires_at.is_some_and(|at| at <= now) {
            return Err("the expiry is not in the future".into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        require_room(&tx, room)?;
        drop_expired(&tx, room, now)?;
        require_owner(&tx, room, key, &by)?;
        let (keys, bytes): (i64, i64) = tx.prepare_cached("SELECT COUNT(*), COALESCE(SUM(length(CAST(line AS BLOB))), 0) FROM room_status WHERE room = ?1 AND key != ?2")
            .and_then(|mut stmt| stmt.query_row(params![room, key], |row| Ok((row.get(0)?, row.get(1)?))))
            .map_err(|_| UNREADABLE)?;
        if keys >= STATUS_MAX_KEYS as i64 || bytes + line.len() as i64 > STATUS_MAX_BYTES as i64 {
            return Err("the room has too much status".into());
        }
        let (by_kind, by_name) = by.parts();
        tx.prepare_cached("INSERT INTO room_status (room, key, line, set_by, set_by_kind, set_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (room, key) DO UPDATE SET line = excluded.line, set_by = excluded.set_by, set_by_kind = excluded.set_by_kind, set_at = excluded.set_at, expires_at = excluded.expires_at")
            .and_then(|mut stmt| stmt.execute(params![room, key, line, by_name, by_kind, now, expires_at]))
            .map_err(|_| UNWRITABLE)?;
        let event = format!("{key}: {line}");
        let seq = append(
            &tx,
            room,
            &author_host,
            &by,
            MessageKind::StatusSet,
            &event,
            now,
            None,
        )?;
        tx.commit().map_err(|_| UNWRITABLE)?;
        Ok(seq)
    }

    /// Clears and logs a status line, returning its event number; `OWNED` is an ownership refusal.
    pub fn clear_status(
        &mut self,
        room: &str,
        key: &str,
        author_host: &str,
        by: &Author,
        now: f64,
    ) -> Result<Option<u64>, String> {
        if !is_key(key, KEY_MAX_CHARS) {
            return Err("not a status key".into());
        }
        let author_host = name(author_host)?;
        let by = clean_author(by)?;
        let now = finite(now)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        require_room(&tx, room)?;
        drop_expired(&tx, room, now)?;
        require_owner(&tx, room, key, &by)?;
        let removed = tx
            .prepare_cached("DELETE FROM room_status WHERE room = ?1 AND key = ?2")
            .and_then(|mut stmt| stmt.execute(params![room, key]))
            .map_err(|_| UNWRITABLE)?;
        let seq = if removed == 0 {
            None
        } else {
            Some(append(
                &tx,
                room,
                &author_host,
                &by,
                MessageKind::StatusClear,
                key,
                now,
                None,
            )?)
        };
        tx.commit().map_err(|_| UNWRITABLE)?;
        Ok(seq)
    }

    /// Lists rooms after `after` in identifier order, capped at `READ_MAX`.
    pub fn rooms(&self, after: Option<&str>, limit: usize) -> Result<Vec<RoomRow>, String> {
        let mut stmt = self.conn.prepare_cached("SELECT id, home, title, created, COALESCE((SELECT MAX(seq) FROM messages WHERE messages.room = rooms.id), 0) FROM rooms WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2")
            .map_err(|_| UNREADABLE)?;
        stmt.query_map(params![after, limit.min(READ_MAX) as i64], room_row)
            .and_then(|rows| rows.collect())
            .map_err(|_| UNREADABLE.to_owned())
    }

    /// Returns a room by identifier, or none when it is missing.
    pub fn room(&self, id: &str) -> Result<Option<RoomRow>, String> {
        let mut stmt = self.conn.prepare_cached("SELECT id, home, title, created, COALESCE((SELECT MAX(seq) FROM messages WHERE messages.room = rooms.id), 0) FROM rooms WHERE id = ?1")
            .map_err(|_| UNREADABLE)?;
        let mut rows = stmt.query_map([id], room_row).map_err(|_| UNREADABLE)?;
        rows.next().transpose().map_err(|_| UNREADABLE.to_owned())
    }

    /// Returns the newest `limit` messages, capped at `READ_MAX`, in ascending sequence order.
    pub fn tail(&self, room: &str, limit: usize) -> Result<Vec<Message>, String> {
        let mut stmt = self.conn.prepare_cached("SELECT seq, kind, author_host, author_kind, author, body, sent_at, delivered_at FROM (SELECT seq, kind, author_host, author_kind, author, body, sent_at, delivered_at FROM messages WHERE room = ?1 ORDER BY seq DESC LIMIT ?2) ORDER BY seq")
            .map_err(|_| UNREADABLE)?;
        stmt.query_map(params![room, limit.min(READ_MAX) as i64], message_row)
            .and_then(|rows| rows.collect())
            .map_err(|_| UNREADABLE.to_owned())
    }

    /// Returns messages strictly after `after`, oldest first, up to `limit` capped at `READ_MAX`.
    pub fn since(&self, room: &str, after: u64, limit: usize) -> Result<Vec<Message>, String> {
        let after = i64::try_from(after).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached("SELECT seq, kind, author_host, author_kind, author, body, sent_at, delivered_at FROM messages WHERE room = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3")
            .map_err(|_| UNREADABLE)?;
        stmt.query_map(
            params![room, after, limit.min(READ_MAX) as i64],
            message_row,
        )
        .and_then(|rows| rows.collect())
        .map_err(|_| UNREADABLE.to_owned())
    }

    /// Lists unexpired status lines in key order, using seconds since the Unix epoch for `now`.
    pub fn status(&self, room: &str, now: f64) -> Result<Vec<StatusLine>, String> {
        let now = finite(now)?;
        let mut stmt = self.conn.prepare_cached("SELECT key, line, set_by_kind, set_by, set_at, expires_at FROM room_status WHERE room = ?1 AND (expires_at IS NULL OR expires_at > ?2) ORDER BY key")
            .map_err(|_| UNREADABLE)?;
        stmt.query_map(params![room, now], |row| {
            Ok(StatusLine {
                key: row.get(0)?,
                line: row.get(1)?,
                set_by: Author::member(row.get::<_, String>(2)?, row.get::<_, String>(3)?),
                set_at: row.get(4)?,
                expires_at: row.get(5)?,
            })
        })
        .and_then(|rows| rows.collect())
        .map_err(|_| UNREADABLE.to_owned())
    }

    /// Replaces an app's whole member list and returns its size.
    pub fn register(
        &mut self,
        app: &str,
        app_version: &str,
        members: &[Author],
        now: f64,
    ) -> Result<usize, String> {
        if !is_key(app, 64) {
            return Err("not an app name".into());
        }
        if app_version.is_empty()
            || app_version.len() > 64
            || !app_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        {
            return Err("not a version".into());
        }
        if members.len() > MEMBERS_MAX {
            return Err("too many members".into());
        }
        let mut cleaned = std::collections::BTreeSet::new();
        for member in members {
            if member.is_human() {
                return Err("not a member kind".into());
            }
            let member = clean_author(member)?;
            cleaned.insert((member.kind, member.name));
        }
        let now = finite(now)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        tx.execute("INSERT INTO apps (app, app_version, registered_at) VALUES (?1, ?2, ?3) ON CONFLICT (app) DO UPDATE SET app_version = excluded.app_version, registered_at = excluded.registered_at", params![app, app_version, now]).map_err(|_| UNWRITABLE)?;
        tx.execute("DELETE FROM members WHERE app = ?1", [app])
            .map_err(|_| UNWRITABLE)?;
        for (kind, name) in &cleaned {
            tx.prepare_cached("INSERT INTO members (app, kind, name) VALUES (?1, ?2, ?3)")
                .and_then(|mut stmt| stmt.execute(params![app, kind, name]))
                .map_err(|_| UNWRITABLE)?;
        }
        tx.commit().map_err(|_| UNWRITABLE)?;
        Ok(cleaned.len())
    }

    /// Humans are members; other authors must occur in a registered list.
    pub fn is_member(&self, author: &Author) -> Result<bool, String> {
        let author = clean_author(author)?;
        if author.is_human() {
            return Ok(true);
        }
        self.conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM members WHERE kind = ?1 AND name = ?2)")
            .and_then(|mut stmt| {
                stmt.query_row(params![author.kind, author.name], |row| row.get(0))
            })
            .map_err(|_| UNREADABLE.to_owned())
    }

    /// Exposes the connection for store tests.
    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn clean_room(
    id: &str,
    home: &str,
    title: &str,
    now: f64,
) -> Result<(String, String, f64), String> {
    if !is_key(id, 64) {
        return Err("not a room id".into());
    }
    let home = name(home)?;
    let now = finite(now)?;
    let title = crate::text::clean(title, 120);
    if title.is_empty() {
        return Err("the room needs a title".into());
    }
    Ok((home, title, now))
}

fn clean_message(
    host: &str,
    author: &Author,
    body: &str,
    now: f64,
) -> Result<(String, Author, String, f64), String> {
    let author_host = name(host)?;
    let author = clean_author(author)?;
    let now = finite(now)?;
    let body = crate::text::clean(body, usize::MAX);
    if body.is_empty() || body.len() > BODY_MAX_BYTES {
        return Err(MESSAGE_INVALID.into());
    }
    Ok((author_host, author, body, now))
}

fn clean_status(
    key: &str,
    line: &str,
    by: &Author,
    now: f64,
    expires_at: Option<f64>,
) -> Result<StatusLine, String> {
    if !is_key(key, KEY_MAX_CHARS) {
        return Err("not a status key".into());
    }
    let by = clean_author(by)?;
    let now = finite(now)?;
    let expires_at = expires_at.map(finite).transpose()?;
    let line = crate::text::clean(line, usize::MAX).replace(['\n', '\t'], " ");
    if line.is_empty() || line.chars().count() > STATUS_LINE_MAX_CHARS {
        return Err("the status line is empty or too long".into());
    }
    Ok(StatusLine {
        key: key.into(),
        line,
        set_by: by,
        set_at: now,
        expires_at,
    })
}

fn name(text: &str) -> Result<String, String> {
    let text = crate::text::clean(text, usize::MAX).replace(['\n', '\t'], " ");
    if text.is_empty() || text.chars().count() > NAME_MAX_CHARS {
        Err("not a name".into())
    } else {
        Ok(text)
    }
}

fn clean_author(author: &Author) -> Result<Author, String> {
    if !is_kind(&author.kind) {
        return Err("not a member kind".into());
    }
    Ok(Author::member(&author.kind, name(&author.name)?))
}

fn room_row(row: &Row) -> rusqlite::Result<RoomRow> {
    Ok(RoomRow {
        id: row.get(0)?,
        home: row.get(1)?,
        title: row.get(2)?,
        created: row.get(3)?,
        last_seq: u64::try_from(row.get::<_, i64>(4)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

fn message_row(row: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        seq: u64::try_from(row.get::<_, i64>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        kind: MessageKind::from_word(&row.get::<_, String>(1)?)
            .ok_or(rusqlite::Error::InvalidQuery)?,
        author_host: row.get(2)?,
        author: Author::member(row.get::<_, String>(3)?, row.get::<_, String>(4)?),
        body: row.get(5)?,
        sent_at: row.get(6)?,
        delivered_at: row.get(7)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn append(
    conn: &Connection,
    room: &str,
    author_host: &str,
    author: &Author,
    kind: MessageKind,
    body: &str,
    now: f64,
    delivered_at: Option<f64>,
) -> Result<u64, String> {
    let last: i64 = conn
        .prepare_cached("SELECT COALESCE(MAX(seq), 0) FROM messages WHERE room = ?1")
        .and_then(|mut stmt| stmt.query_row([room], |row| row.get(0)))
        .map_err(|_| UNREADABLE)?;
    let seq = last
        .checked_add(1)
        .filter(|seq| *seq > 0)
        .ok_or("the room has too many messages")?;
    let (author_kind, author_name) = author.parts();
    conn.prepare_cached("INSERT INTO messages (room, seq, kind, author_host, author_kind, author, body, sent_at, delivered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")
        .and_then(|mut stmt| stmt.execute(params![room, seq, kind.word(), author_host, author_kind, author_name, body, now, delivered_at]))
        .map_err(|_| UNWRITABLE)?;
    Ok(seq as u64)
}

fn drop_expired(conn: &Connection, room: &str, now: f64) -> Result<(), String> {
    conn.prepare_cached("DELETE FROM room_status WHERE room = ?1 AND expires_at <= ?2")
        .and_then(|mut stmt| stmt.execute(params![room, now]))
        .map(|_| ())
        .map_err(|_| UNWRITABLE.to_owned())
}

fn require_owner(conn: &Connection, room: &str, key: &str, by: &Author) -> Result<(), String> {
    if by.is_human() {
        return Ok(());
    };
    let owner: Option<(String, String)> = conn
        .prepare_cached("SELECT set_by_kind, set_by FROM room_status WHERE room = ?1 AND key = ?2")
        .and_then(|mut stmt| {
            let mut rows =
                stmt.query_map(params![room, key], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.next().transpose()
        })
        .map_err(|_| UNREADABLE)?;
    match owner {
        Some((kind, name)) if kind != by.kind || name != by.name => Err(OWNED.into()),
        _ => Ok(()),
    }
}

fn finite(at: f64) -> Result<f64, String> {
    if at.is_finite() {
        Ok(at)
    } else {
        Err("not a time".into())
    }
}

fn is_key(key: &str, max: usize) -> bool {
    !key.is_empty()
        && key.len() <= max
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn require_room(conn: &Connection, room: &str) -> Result<(), String> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM rooms WHERE id = ?1)")
        .and_then(|mut stmt| stmt.query_row([room], |row| row.get(0)))
        .map_err(|_| UNREADABLE)?;
    if exists {
        Ok(())
    } else {
        Err("no such room".into())
    }
}

/// Checks whether an existing store needs a newer build, without migrating or writing it.
pub fn store_is_newer(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(UNREADABLE.into()),
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(UNREADABLE.into()),
    }
    let path = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)))
        .ok_or(UNREADABLE)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| UNREADABLE)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|_| UNREADABLE)?;
    match schema_version(&conn) {
        Ok(_) => Ok(false),
        Err(e) if e == NEWER => Ok(true),
        Err(e) => Err(e),
    }
}

fn schema_version(conn: &Connection) -> Result<usize, String> {
    let exists: bool = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version')")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get(0)))
        .map_err(|_| UNREADABLE)?;
    if !exists {
        return Ok(0);
    }
    let (count, version): (i64, i64) = conn
        .prepare_cached("SELECT COUNT(*), COALESCE(MIN(version), 0) FROM schema_version")
        .and_then(|mut stmt| stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(|_| UNREADABLE)?;
    if count != 1 || version < 1 {
        return Err("the room store schema is not supported".into());
    }
    if version > MIGRATIONS.len() as i64 {
        return Err(NEWER.into());
    }
    Ok(version as usize)
}

const KIND_CHECK: &str =
    "{col} GLOB '[a-z]*' AND {col} NOT GLOB '*[^a-z0-9_-]*' AND length({col}) <= 40";

fn migration_sql(version: usize) -> String {
    let mut sql = MIGRATIONS[version].to_owned();
    for col in ["author_kind", "set_by_kind", "kind"] {
        sql = sql.replace(&format!("@{col}@"), &KIND_CHECK.replace("{col}", col));
    }
    sql
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| UNWRITABLE)?;
    let version = schema_version(&tx)?;
    if version == MIGRATIONS.len() {
        return Ok(());
    }
    for pending in version..MIGRATIONS.len() {
        tx.execute_batch(&migration_sql(pending))
            .map_err(|_| "the room store could not be migrated")?;
    }
    tx.execute("DELETE FROM schema_version", [])
        .map_err(|_| UNWRITABLE)?;
    tx.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        [MIGRATIONS.len() as i64],
    )
    .map_err(|_| UNWRITABLE)?;
    tx.commit().map_err(|_| UNWRITABLE.to_owned())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
