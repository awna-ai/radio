use super::*;
use rusqlite::limits::Limit;
use std::os::unix::fs::MetadataExt;

const CHANGE_SOURCE: &str = "the store to import can be changed by another user";
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
    /// This source was imported before; nothing was copied.
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

impl RoomStore {
    /// Copies the rooms, messages and status lines of an older store at `source` into this one, once per source path.
    ///
    /// This code never writes the source, but SQLite may create `-wal` and `-shm` beside a WAL source when opening it.
    /// Version-1 status lines are read as set by the human, matching this store's migration 2 default.
    pub fn import_rooms(&mut self, source: &Path, now: f64) -> Result<Import, String> {
        self.import_rooms_with_deadline(source, now, Duration::from_secs(30), 1_000_000)
    }

    pub(super) fn import_rooms_with_deadline(
        &mut self,
        source: &Path,
        now: f64,
        deadline: Duration,
        message_cap: u64,
    ) -> Result<Import, String> {
        let began = std::time::Instant::now();
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
        canonical
            .to_str()
            .ok_or("the store to import is not a plain path")?;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&canonical)
            .map_err(|_| OPEN_SOURCE)?;
        let meta = file.metadata().map_err(|_| OPEN_SOURCE)?;
        // SAFETY: geteuid takes no arguments and has no preconditions.
        if !owner_only(&meta, unsafe { libc::geteuid() }) {
            return Err(CHANGE_SOURCE.into());
        }
        let own_file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
            .map_err(|_| OPEN_SOURCE)?;
        let own_meta = own_file.metadata().map_err(|_| OPEN_SOURCE)?;
        if (meta.dev(), meta.ino()) == (own_meta.dev(), own_meta.ino()) {
            return Err("the store to import is this store".into());
        }
        let canonical = opened_path(&file, &canonical)?;
        // SQLite reopens the source by path and may read these side files.
        // A sticky folder still lets another user plant a side file first.
        for folder in canonical.ancestors().skip(1) {
            let folder = fs::symlink_metadata(folder).map_err(|_| CHANGE_SOURCE)?;
            // SAFETY: geteuid takes no arguments and has no preconditions.
            if !folder.is_dir()
                || !folder_safe(folder.uid(), folder.gid(), folder.mode(), unsafe {
                    libc::geteuid()
                })
            {
                return Err(CHANGE_SOURCE.into());
            }
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut path = canonical.as_os_str().to_os_string();
            path.push(suffix);
            match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(side) => {
                    let meta = side.metadata().map_err(|_| CHANGE_SOURCE)?;
                    // SAFETY: geteuid takes no arguments and has no preconditions.
                    if !owner_only(&meta, unsafe { libc::geteuid() }) {
                        return Err(CHANGE_SOURCE.into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(CHANGE_SOURCE.into()),
            }
        }
        let key = canonical
            .to_str()
            .ok_or("the store to import is not a plain path")?;
        if already_imported(&self.conn, key, &meta)? {
            return Ok(Import::AlreadyImported);
        }
        let source = Connection::open_with_flags(
            &canonical,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| OPEN_SOURCE)?;
        let reopened = fs::symlink_metadata(&canonical).map_err(|_| OPEN_SOURCE)?;
        if !reopened.is_file() || (reopened.dev(), reopened.ino()) != (meta.dev(), meta.ino()) {
            return Err(OPEN_SOURCE.into());
        }
        source.busy_timeout(BUSY_TIMEOUT).map_err(|_| OPEN_SOURCE)?;
        source
            .pragma_update(None, "trusted_schema", false)
            .map_err(|_| READ_SOURCE)?;
        source
            .pragma_update(None, "cell_size_check", true)
            .map_err(|_| READ_SOURCE)?;
        source
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, 1 << 20)
            .map_err(|_| READ_SOURCE)?;
        source
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 << 10)
            .map_err(|_| READ_SOURCE)?;
        source
            .progress_handler(10_000, Some(move || began.elapsed() >= deadline))
            .map_err(|_| READ_SOURCE)?;
        // Keep every source read in the same snapshot, including its schema version.
        let snapshot = source.unchecked_transaction().map_err(|_| READ_SOURCE)?;
        let version = schema_version(&snapshot).map_err(|_| {
            if began.elapsed() >= deadline {
                READ_SOURCE
            } else {
                VERSION_SOURCE
            }
        })?;
        if !matches!(version, 1 | 2) {
            return Err(VERSION_SOURCE.into());
        }
        for table in ["rooms", "messages", "room_status"] {
            let is_table: bool = snapshot
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1 AND type = 'table')",
                    [table],
                    |r| r.get(0),
                )
                .map_err(|_| READ_SOURCE)?;
            if !is_table {
                return Err(VERSION_SOURCE.into());
            }
        }
        let mut rooms_stmt = snapshot
            .prepare("SELECT id, home, title, created, 0 FROM rooms ORDER BY id")
            .map_err(|_| READ_SOURCE)?;
        let rooms = rooms_stmt
            .query_map([], room_row)
            .map_err(|_| READ_SOURCE)?;
        let mut messages_stmt = snapshot.prepare(if version == 1 {
            "SELECT author_host, author_kind, author, body, sent_at, delivered_at, 'post' FROM messages WHERE room = ?1 ORDER BY seq"
        } else {
            "SELECT author_host, author_kind, author, body, sent_at, delivered_at, kind FROM messages WHERE room = ?1 ORDER BY seq"
        }).map_err(|_| READ_SOURCE)?;
        let mut statuses_stmt = snapshot.prepare(if version == 1 {
            "SELECT key, line, 'human', set_by, set_at, expires_at FROM room_status WHERE room = ?1 ORDER BY key"
        } else {
            "SELECT key, line, set_by_kind, set_by, set_at, expires_at FROM room_status WHERE room = ?1 ORDER BY key"
        }).map_err(|_| READ_SOURCE)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| UNWRITABLE)?;
        // A concurrent installer may have finished while this one read the source.
        if already_imported(&tx, key, &meta)? {
            return Ok(Import::AlreadyImported);
        }
        let mut report = ImportReport {
            rooms_added: 0,
            rooms_merged: 0,
            messages: 0,
            statuses: 0,
            skipped: 0,
        };
        let mut messages_seen = 0u64;
        for room in rooms {
            let room = room.map_err(|_| READ_SOURCE)?;
            let cleaned_room = clean_room(&room.id, &room.home, &room.title, room.created);
            let valid_room = cleaned_room.is_ok();
            if let Ok((home, title, created)) = cleaned_room {
                let added = tx.execute("INSERT INTO rooms (id, home, title, created) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING", params![room.id, home, title, created])
                    .map_err(|_| UNWRITABLE)?;
                if added == 1 {
                    report.rooms_added += 1;
                } else {
                    report.rooms_merged += 1;
                }
            } else {
                report.skipped += 1;
            }
            let messages = messages_stmt
                .query_map([&room.id], source_message)
                .map_err(|_| READ_SOURCE)?;
            for message in messages {
                let message = message.map_err(|_| READ_SOURCE)?;
                messages_seen += 1;
                if messages_seen > message_cap {
                    return Err("the store to import is too large".into());
                }
                if !valid_room {
                    report.skipped += 1;
                    continue;
                }
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
            let statuses = statuses_stmt
                .query_map([&room.id], source_status)
                .map_err(|_| READ_SOURCE)?;
            for status in statuses {
                let status = status.map_err(|_| READ_SOURCE)?;
                if !valid_room {
                    report.skipped += 1;
                    continue;
                }
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

fn already_imported(conn: &Connection, key: &str, meta: &fs::Metadata) -> Result<bool, String> {
    let mut stmt = conn
        .prepare("SELECT source FROM imports")
        .map_err(|_| UNWRITABLE)?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| UNWRITABLE)?;
    for path in paths {
        let path = path.map_err(|_| UNWRITABLE)?;
        if path == key {
            return Ok(true);
        }
        // F_GETPATH normalizes case, but can still name a different hard link.
        if let Ok(file) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            let previous = file.metadata().map_err(|_| OPEN_SOURCE)?;
            if (previous.dev(), previous.ino()) == (meta.dev(), meta.ino()) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn source_message(r: &Row<'_>) -> rusqlite::Result<SourceMessage> {
    Ok(SourceMessage {
        host: r.get(0)?,
        author: Author::member(r.get::<_, String>(1)?, r.get::<_, String>(2)?),
        body: r.get(3)?,
        sent_at: r.get(4)?,
        delivered_at: r.get(5)?,
        kind: r.get(6)?,
    })
}

fn source_status(r: &Row<'_>) -> rusqlite::Result<StatusLine> {
    Ok(StatusLine {
        key: r.get(0)?,
        line: r.get(1)?,
        set_by: Author::member(r.get::<_, String>(2)?, r.get::<_, String>(3)?),
        set_at: r.get(4)?,
        expires_at: r.get(5)?,
    })
}

fn folder_safe(owner: u32, group: u32, mode: u32, uid: u32) -> bool {
    (owner == uid || owner == 0)
        && (mode & 0o020 == 0 || matches!(group, 0 | 80))
        && mode & 0o002 == 0
}

fn owner_only(meta: &fs::Metadata, uid: u32) -> bool {
    meta.is_file() && meta.uid() == uid && meta.mode() & 0o022 == 0
}

#[cfg(target_vendor = "apple")]
fn opened_path(file: &fs::File, _fallback: &Path) -> Result<PathBuf, String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: file owns a valid descriptor and buf is writable for PATH_MAX bytes,
    // as F_GETPATH requires for its NUL-terminated path.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
        return Err(OPEN_SOURCE.into());
    }
    let end = buf.iter().position(|byte| *byte == 0).ok_or(OPEN_SOURCE)?;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..end])))
}

#[cfg(not(target_vendor = "apple"))]
fn opened_path(_file: &fs::File, fallback: &Path) -> Result<PathBuf, String> {
    Ok(fallback.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_folder_permissions() {
        for (owner, group, mode, allowed) in [
            (501, 20, 0o755, true),
            (0, 0, 0o777, false),
            (0, 0, 0o1777, false),
            (501, 20, 0o775, false),
            (0, 80, 0o775, true),
            (0, 0, 0o775, true),
            (502, 20, 0o755, false),
        ] {
            assert_eq!(folder_safe(owner, group, mode, 501), allowed);
        }
    }

    #[test]
    fn another_owner_is_refused() {
        let meta =
            fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/import.rs")).unwrap();
        assert!(!owner_only(&meta, meta.uid().wrapping_add(1)));
        assert!(owner_only(&meta, meta.uid()));
    }
}
