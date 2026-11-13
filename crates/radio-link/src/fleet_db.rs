use crate::{
    code::{Hints, JoinCode},
    fleet::{self, FleetRecord},
    key, wire,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub const DB_FILE: &str = "fleet.db";
pub const SCHEMA_VERSION: i64 = 1;
pub const CODES_MAX: usize = 64;
pub const TTL_DEFAULT: u64 = 30 * 60;
pub const TTL_MIN: u64 = 60;
pub const TTL_MAX: u64 = 24 * 60 * 60;
pub const RECOVERY_FILE: &str = "recovery.key";
const READ: &str = "fleet.db could not be read";
const WRITE: &str = "fleet.db could not be written";
const OPEN: &str = "fleet.db could not be opened";
const RECOVERY_WRITE: &str = "the recovery key could not be written";
const NOT_LIVE: &str = "not a live join code";
const SCHEMA: &str = "
CREATE TABLE fleet (
    one INTEGER PRIMARY KEY CHECK (one = 1),
    record TEXT NOT NULL
) STRICT;
CREATE TABLE codes (
    id TEXT PRIMARY KEY CHECK (length(id) = 16),
    hash BLOB NOT NULL UNIQUE CHECK (length(hash) = 32),
    name TEXT,
    created REAL NOT NULL,
    expires_at REAL NOT NULL,
    uses INTEGER NOT NULL CHECK (uses >= 1)
) STRICT;
";

#[derive(Debug, Clone, PartialEq)]
pub struct CodeRow {
    pub id: String,
    pub name: Option<String>,
    pub created: f64,
    pub expires_at: f64,
    pub uses: u32,
}

pub struct FleetDb {
    conn: Connection,
}

impl FleetDb {
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let parent = path.parent().ok_or("the fleet folder is not usable")?;
        let _folder = key::private_folder(parent).map_err(|_| "the fleet folder is not usable")?;
        let path = parent
            .canonicalize()
            .map_err(|_| OPEN)?
            .join(path.file_name().ok_or(OPEN)?);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| OPEN)?;
        let metadata = file.metadata().map_err(|_| OPEN)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(OPEN);
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| OPEN)?;
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| OPEN)?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|_| READ)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| WRITE)?;
        conn.pragma_update(None, "trusted_schema", false)
            .map_err(|_| WRITE)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn fleet(&self) -> Result<Option<FleetRecord>, &'static str> {
        let text: Option<String> = self
            .conn
            .query_row("SELECT record FROM fleet WHERE one = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| READ)?;
        text.map(|text| {
            let damaged = "fleet.db holds a damaged fleet record";
            let record: FleetRecord = serde_json::from_str(&text).map_err(|_| damaged)?;
            record.verify().map_err(|_| damaged)?;
            Ok(record)
        })
        .transpose()
    }

    pub fn found(
        &mut self,
        node: &iroh::SecretKey,
        name: &str,
        link_dir: &Path,
    ) -> Result<(FleetRecord, PathBuf), &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let exists: bool = tx
            .query_row("SELECT EXISTS(SELECT 1 FROM fleet)", [], |row| row.get(0))
            .map_err(|_| READ)?;
        if exists {
            return Err("already in a fleet");
        }
        let (record, recovery) = fleet::found(node, name)?;
        let _folder = key::private_folder(link_dir).map_err(|_| "the link folder is not usable")?;
        let path = link_dir.join(RECOVERY_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "a recovery key file is waiting to be moved"
                } else {
                    RECOVERY_WRITE
                }
            })?;
        key::exclude_from_backup(&file);
        let result = (|| {
            let secret = data_encoding::HEXLOWER.encode(&recovery.to_bytes());
            file.write_all(secret.as_bytes())
                .map_err(|_| RECOVERY_WRITE)?;
            file.write_all(b"\n").map_err(|_| RECOVERY_WRITE)?;
            file.sync_all().map_err(|_| RECOVERY_WRITE)?;
            let text = serde_json::to_string(&record).map_err(|_| WRITE)?;
            tx.execute("INSERT INTO fleet (one, record) VALUES (1, ?1)", [text])
                .map_err(|_| WRITE)?;
            tx.commit().map_err(|_| WRITE)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok((record, path))
    }

    pub fn create_code(
        &mut self,
        node: &iroh::SecretKey,
        name: Option<&str>,
        ttl: Option<u64>,
        hints: Hints,
        now: f64,
    ) -> Result<(JoinCode, CodeRow), &'static str> {
        let record = self.fleet()?.ok_or("not in a fleet")?;
        if name.is_some_and(|name| !wire::member_name(name)) {
            return Err("not a code name");
        }
        let ttl = ttl.unwrap_or(TTL_DEFAULT);
        if !(TTL_MIN..=TTL_MAX).contains(&ttl) {
            return Err("not a code lifetime");
        }
        let code = JoinCode::new(record.body.id, node.public(), hints)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        drop_expired(&tx, now)?;
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM codes", [], |row| row.get(0))
            .map_err(|_| READ)?;
        if count >= CODES_MAX as i64 {
            return Err("too many open join codes");
        }
        let mut id = [0; 8];
        getrandom::fill(&mut id).map_err(|_| "the join code could not be made")?;
        let row = CodeRow {
            id: data_encoding::HEXLOWER.encode(&id),
            name: name.map(str::to_owned),
            created: now,
            expires_at: now + ttl as f64,
            uses: 1,
        };
        tx.execute("INSERT INTO codes (id, hash, name, created, expires_at, uses) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![row.id, code.hash().as_slice(), row.name, row.created, row.expires_at, row.uses]).map_err(|_| WRITE)?;
        tx.commit().map_err(|_| WRITE)?;
        Ok((code, row))
    }

    pub fn codes(&mut self, now: f64) -> Result<Vec<CodeRow>, &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        drop_expired(&tx, now)?;
        let rows = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, name, created, expires_at, uses FROM codes ORDER BY created, id",
                )
                .map_err(|_| READ)?;
            stmt.query_map([], code_row)
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .map_err(|_| READ)?
        };
        tx.commit().map_err(|_| WRITE)?;
        Ok(rows)
    }

    pub fn revoke_code(&mut self, id: &str) -> Result<bool, &'static str> {
        if id.len() != 16
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("not a code id");
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let changed = tx
            .execute("DELETE FROM codes WHERE id = ?1", [id])
            .map_err(|_| WRITE)?;
        tx.commit().map_err(|_| WRITE)?;
        Ok(changed != 0)
    }

    pub fn join_open(&self, now: f64) -> Result<bool, &'static str> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM codes WHERE expires_at > ?1)",
                [now],
                |row| row.get(0),
            )
            .map_err(|_| READ)
    }

    pub fn consume_code(
        &mut self,
        code: &JoinCode,
        own: &iroh::EndpointId,
        now: f64,
    ) -> Result<CodeRow, &'static str> {
        if code.inviter != *own
            || self
                .fleet()?
                .is_none_or(|record| record.body.id != code.fleet)
        {
            return Err(NOT_LIVE);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let hash = code.hash();
        let mut matched = None;
        {
            let mut stmt = tx.prepare("SELECT id, name, created, expires_at, uses, hash FROM codes WHERE expires_at > ?1").map_err(|_| READ)?;
            let mut rows = stmt.query([now]).map_err(|_| READ)?;
            while let Some(row) = rows.next().map_err(|_| READ)? {
                let stored: Vec<u8> = row.get(5).map_err(|_| READ)?;
                if same(&hash, &stored) {
                    matched = Some(code_row(row).map_err(|_| READ)?);
                }
            }
        }
        let row = matched.ok_or(NOT_LIVE)?;
        if row.uses == 1 {
            tx.execute("DELETE FROM codes WHERE id = ?1", [&row.id])
                .map_err(|_| WRITE)?;
        } else {
            tx.execute("UPDATE codes SET uses = uses - 1 WHERE id = ?1", [&row.id])
                .map_err(|_| WRITE)?;
        }
        tx.commit().map_err(|_| WRITE)?;
        Ok(row)
    }
}

fn code_row(row: &Row<'_>) -> rusqlite::Result<CodeRow> {
    Ok(CodeRow {
        id: row.get(0)?,
        name: row.get(1)?,
        created: row.get(2)?,
        expires_at: row.get(3)?,
        uses: row.get(4)?,
    })
}

fn drop_expired(conn: &Connection, now: f64) -> Result<(), &'static str> {
    conn.execute("DELETE FROM codes WHERE expires_at <= ?1", [now])
        .map(|_| ())
        .map_err(|_| WRITE)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    let mut difference = 0;
    for index in 0..32 {
        difference |= a[index] ^ b[index];
    }
    difference == 0
}

fn migrate(conn: &mut Connection) -> Result<(), &'static str> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| WRITE)?;
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| READ)?;
    match version {
        0 => {
            tx.execute_batch(SCHEMA).map_err(|_| WRITE)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|_| WRITE)?;
            tx.commit().map_err(|_| WRITE)
        }
        SCHEMA_VERSION => Ok(()),
        value if value > SCHEMA_VERSION => Err("fleet.db is newer than this build"),
        _ => Err(READ),
    }
}

#[cfg(test)]
#[path = "fleet_db_tests.rs"]
mod tests;
