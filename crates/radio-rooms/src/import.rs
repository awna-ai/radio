use super::*;

const OPEN_SOURCE: &str = "the store to import could not be opened";
const READ_SOURCE: &str = "the store to import could not be read";
const VERSION_SOURCE: &str = "the store to import is not a version this build can import";

/// What one import brought in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Rooms created because this store had no room with that id.
    pub rooms_added: u64,
    /// Rooms already here, whose messages were appended after this store's own.
    pub rooms_merged: u64,
    /// Messages copied.
    pub messages: u64,
    /// Status lines copied.
    pub statuses: u64,
    /// Rows refused by this store's rules, counted but never copied.
    pub skipped: u64,
}

/// The outcome of [`RoomStore::import_rooms`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Import {
    /// The source was imported.
    Imported(ImportReport),
    /// This source was imported before; nothing was read or written.
    AlreadyImported,
}

struct SourceMessage {
    host: String,
    author: Author,
    body: String,
    sent_at: f64,
    delivered_at: Option<f64>,
    kind: String,
}

struct SourceRoom {
    room: RoomRow,
    messages: Vec<SourceMessage>,
    statuses: Vec<StatusLine>,
}

impl RoomStore {
    /// Copies the rooms, messages and status lines of an older store at `source` into this one, once per source path.
    pub fn import_rooms(&mut self, source: &Path, now: f64) -> Result<Import, String> {
        let now = finite(now)?;
        if !source.is_absolute() {
            return Err("the store to import is not an absolute path".into());
        }
        let canonical = source
            .parent()
            .ok_or(OPEN_SOURCE)?
            .canonicalize()
            .map_err(|_| OPEN_SOURCE)?
            .join(source.file_name().ok_or(OPEN_SOURCE)?);
        let key = canonical
            .to_str()
            .ok_or("the store to import is not a plain path")?;
        if already_imported(&self.conn, key)? {
            return Ok(Import::AlreadyImported);
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical)
            .map_err(|_| OPEN_SOURCE)?;
        if !file.metadata().map_err(|_| OPEN_SOURCE)?.is_file() {
            return Err("the store to import is not a file".into());
        }
        drop(file);
        let source = Connection::open_with_flags(
            &canonical,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| OPEN_SOURCE)?;
        source.busy_timeout(BUSY_TIMEOUT).map_err(|_| OPEN_SOURCE)?;
        // Keep every source read in the same snapshot, including its schema version.
        let snapshot = source.unchecked_transaction().map_err(|_| READ_SOURCE)?;
        let version = schema_version(&snapshot).map_err(|_| VERSION_SOURCE)?;
        if !matches!(version, 1 | 2) {
            return Err(VERSION_SOURCE.into());
        }
        let rooms = read_source(&snapshot, version).map_err(|_| READ_SOURCE)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        // A concurrent installer may have finished while this one read the source.
        if already_imported(&tx, key)? {
            return Ok(Import::AlreadyImported);
        }
        let mut report = ImportReport {
            rooms_added: 0,
            rooms_merged: 0,
            messages: 0,
            statuses: 0,
            skipped: 0,
        };
        for SourceRoom {
            room,
            messages,
            statuses,
        } in rooms
        {
            let Ok((home, title, created)) =
                clean_room(&room.id, &room.home, &room.title, room.created)
            else {
                report.skipped += 1 + messages.len() as u64 + statuses.len() as u64;
                continue;
            };
            let added = tx.execute("INSERT INTO rooms (id, home, title, created) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING", params![room.id, home, title, created])
                .map_err(|_| UNWRITABLE)?;
            if added == 1 {
                report.rooms_added += 1;
            } else {
                report.rooms_merged += 1;
            }
            for message in messages {
                let cleaned = clean_message(
                    &message.host,
                    &message.author,
                    &message.body,
                    message.sent_at,
                );
                let delivered = message.delivered_at.map(finite).transpose();
                let (Ok((host, author, body, sent_at)), Ok(delivered), Some(kind)) =
                    (cleaned, delivered, MessageKind::from_word(&message.kind))
                else {
                    report.skipped += 1;
                    continue;
                };
                append(
                    &tx, &room.id, &host, &author, kind, &body, sent_at, delivered,
                )
                .map_err(|_| UNWRITABLE)?;
                report.messages += 1;
            }
            for status in statuses {
                let Ok(status) = clean_status(
                    &status.key,
                    &status.line,
                    &status.set_by,
                    status.set_at,
                    status.expires_at,
                ) else {
                    report.skipped += 1;
                    continue;
                };
                if status.expires_at.is_some_and(|at| at <= now) {
                    continue;
                }
                report.statuses += tx.execute("INSERT INTO room_status (room, key, line, set_by, set_by_kind, set_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (room, key) DO NOTHING",
                    params![room.id, status.key, status.line, status.set_by.name, status.set_by.kind, status.set_at, status.expires_at])
                    .map_err(|_| UNWRITABLE)? as u64;
            }
        }
        tx.execute("INSERT INTO imports (source, imported_at, rooms, messages, statuses, skipped) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![key, now, (report.rooms_added + report.rooms_merged) as i64, report.messages as i64, report.statuses as i64, report.skipped as i64])
            .map_err(|_| UNWRITABLE)?;
        tx.commit().map_err(|_| UNWRITABLE)?;
        Ok(Import::Imported(report))
    }
}

fn already_imported(conn: &Connection, key: &str) -> Result<bool, String> {
    conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM imports WHERE source = ?1)")
        .and_then(|mut stmt| stmt.query_row([key], |r| r.get(0)))
        .map_err(|_| UNWRITABLE.into())
}

fn read_source(conn: &Connection, version: usize) -> rusqlite::Result<Vec<SourceRoom>> {
    let rooms = conn
        .prepare("SELECT id, home, title, created, 0 FROM rooms ORDER BY id")?
        .query_map([], room_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut messages = conn.prepare(if version == 1 {
        "SELECT author_host, author_kind, author, body, sent_at, delivered_at, 'post' FROM messages WHERE room = ?1 ORDER BY seq"
    } else {
        "SELECT author_host, author_kind, author, body, sent_at, delivered_at, kind FROM messages WHERE room = ?1 ORDER BY seq"
    })?;
    let mut statuses = conn.prepare(if version == 1 {
        "SELECT key, line, 'human', set_by, set_at, expires_at FROM room_status WHERE room = ?1 ORDER BY key"
    } else {
        "SELECT key, line, set_by_kind, set_by, set_at, expires_at FROM room_status WHERE room = ?1 ORDER BY key"
    })?;
    rooms
        .into_iter()
        .map(|room| {
            let messages = messages
                .query_map([&room.id], |r| {
                    Ok(SourceMessage {
                        host: r.get(0)?,
                        author: Author::member(r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                        body: r.get(3)?,
                        sent_at: r.get(4)?,
                        delivered_at: r.get(5)?,
                        kind: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            let statuses = statuses
                .query_map([&room.id], |r| {
                    Ok(StatusLine {
                        key: r.get(0)?,
                        line: r.get(1)?,
                        set_by: Author::member(r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                        set_at: r.get(4)?,
                        expires_at: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(SourceRoom {
                room,
                messages,
                statuses,
            })
        })
        .collect()
}
