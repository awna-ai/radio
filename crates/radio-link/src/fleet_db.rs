use crate::{
    admit::Standing,
    code::{Hints, JoinCode},
    fleet::{self, FleetRecord},
    key,
    membership::{self, COMPUTERS_MAX, JoinAnswer, Known, REMOVED_MAX, RecordBody, SignedRecord},
    wire,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub const DB_FILE: &str = "fleet.db";
pub const SCHEMA_VERSION: i64 = 2;
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
const SCHEMA_1: &str = "
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

const SCHEMA_2: &str = "
CREATE TABLE local (
    one INTEGER PRIMARY KEY CHECK (one = 1),
    next_counter INTEGER NOT NULL CHECK (next_counter >= 1),
    invited_by TEXT CHECK (invited_by IS NULL OR length(invited_by) = 64)
) STRICT;
CREATE TABLE computers (
    key TEXT PRIMARY KEY CHECK (length(key) = 64),
    name TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'pending', 'blocked')),
    invited_by TEXT CHECK (invited_by IS NULL OR length(invited_by) = 64),
    trusted INTEGER NOT NULL CHECK (trusted IN (0, 1)),
    addrs TEXT NOT NULL,
    added REAL NOT NULL
) STRICT;
CREATE TABLE tombstones (
    key TEXT PRIMARY KEY CHECK (length(key) = 64),
    name TEXT,
    signer TEXT NOT NULL CHECK (length(signer) = 64),
    at REAL NOT NULL
) STRICT;
CREATE TABLE held (
    signer TEXT NOT NULL CHECK (length(signer) = 64),
    subject TEXT NOT NULL CHECK (length(subject) = 64),
    at REAL NOT NULL,
    PRIMARY KEY (signer, subject)
) STRICT;
CREATE TABLE records (
    signer TEXT NOT NULL CHECK (length(signer) = 64),
    counter INTEGER NOT NULL CHECK (counter >= 1),
    kind TEXT NOT NULL CHECK (kind IN ('introduction', 'removal')),
    at REAL NOT NULL,
    PRIMARY KEY (signer, counter)
) STRICT;
";

pub const INTRODUCTIONS_PER_DAY: i64 = 3;
pub const DAY: f64 = 86400.0;
pub const HELD_PER_SIGNER: i64 = 64;
pub const HELD_MAX: i64 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Accepted,
    Pending,
    Blocked,
    Removed,
    Left,
}
impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Removed => "removed",
            Self::Left => "left",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Computer {
    pub key: String,
    pub name: Option<String>,
    pub state: State,
    pub invited_by: Option<String>,
    pub trusted: bool,
    pub addrs: Vec<std::net::SocketAddr>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Added { pending: bool },
    Held,
    Removed(iroh::EndpointId),
    LeftFleet,
}
pub type Recipient = (iroh::EndpointId, Vec<std::net::SocketAddr>);

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
        load_fleet(&self.conn)
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
            tx.execute("INSERT INTO local VALUES (1, 1, NULL)", [])
                .map_err(|_| WRITE)?;
            tx.commit().map_err(|_| WRITE)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok((record, path))
    }

    pub fn standing(&self, key: &iroh::EndpointId) -> Result<Standing, &'static str> {
        let key = key.to_string();
        if tombstoned(&self.conn, &key)? {
            return Ok(Standing::Refused);
        }
        Ok(match find_computer(&self.conn, &key)?.map(|c| c.state) {
            Some(State::Accepted) => Standing::Accepted,
            Some(State::Blocked) => Standing::Refused,
            _ => Standing::Unknown,
        })
    }
    pub fn computer(&self, target: &str) -> Result<Computer, &'static str> {
        load_fleet(&self.conn)?.ok_or("not in a fleet")?;
        resolve(&self.conn, target)
    }
    pub fn computers(&self) -> Result<Vec<Computer>, &'static str> {
        let mut computers = all_computers(&self.conn)?;
        let mut stmt = self
            .conn
            .prepare("SELECT key, name, signer FROM tombstones ORDER BY key")
            .map_err(|_| READ)?;
        let removed = stmt
            .query_map([], |r| {
                let key: String = r.get(0)?;
                let signer: String = r.get(2)?;
                Ok(Computer {
                    state: if signer == key {
                        State::Left
                    } else {
                        State::Removed
                    },
                    key,
                    name: r.get(1)?,
                    invited_by: None,
                    trusted: false,
                    addrs: vec![],
                })
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|_| READ)?;
        computers.extend(removed);
        Ok(computers)
    }
    pub fn recipients(&self, except: &iroh::EndpointId) -> Result<Vec<Recipient>, &'static str> {
        recipients(&self.conn, except)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit(
        &mut self,
        node: &iroh::SecretKey,
        own_name: &str,
        own_addrs: &[SocketAddr],
        code: &JoinCode,
        newcomer: &iroh::EndpointId,
        name: &str,
        addrs: &[SocketAddr],
        consent: &str,
        now: f64,
    ) -> Result<(JoinAnswer, Option<SignedRecord>), &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let fleet = load_fleet(&tx)?.ok_or("not in a fleet")?;
        let own = node.public().to_string();
        let key = newcomer.to_string();
        if code.inviter != node.public() || code.fleet != fleet.body.id {
            return Err(NOT_LIVE);
        }
        if !wire::member_name(name) {
            return Err("not a computer name");
        }
        if !membership::valid_addrs(addrs, false) {
            return Err("the computer's addresses are not usable");
        }
        if !membership::consent_holds(consent, &fleet.body.id, &own, &key, name, addrs) {
            return Err("the join request is not signed");
        }
        let current = find_computer(&tx, &key)?;
        if newcomer == &node.public()
            || tombstoned(&tx, &key)?
            || current.as_ref().is_some_and(|c| c.state == State::Blocked)
        {
            return Err("not let in");
        }
        if name == own_name || name_taken(&tx, name, &key)? {
            return Err("the name is taken");
        }
        consume(&tx, code, now)?;
        let introduction = if current.is_some() {
            tx.execute(
                "UPDATE computers SET state = 'accepted', name = ?2, addrs = ?3 WHERE key = ?1",
                params![key, name, serde_json::to_string(addrs).map_err(|_| WRITE)?],
            )
            .map_err(|_| WRITE)?;
            None
        } else {
            if introductions(&tx, &own, now)? >= INTRODUCTIONS_PER_DAY {
                return Err("too many introductions today");
            }
            insert_computer(
                &tx,
                &Known {
                    key: key.clone(),
                    name: name.into(),
                    invited_by: Some(own.clone()),
                    addrs: addrs.to_vec(),
                },
                State::Accepted,
                now,
            )?;
            let counter = take_counter(&tx)?;
            seen(&tx, &own, counter, "introduction", now)?;
            Some(membership::sign(
                RecordBody::Introduction {
                    fleet: fleet.body.id.clone(),
                    inviter: own.clone(),
                    counter,
                    newcomer: key.clone(),
                    name: name.into(),
                    addrs: addrs.to_vec(),
                    consent: consent.into(),
                },
                node,
            ))
        };
        let invited_by = tx
            .query_row("SELECT invited_by FROM local WHERE one = 1", [], |r| {
                r.get(0)
            })
            .map_err(|_| READ)?;
        let mut computers = vec![Known {
            key: own,
            name: own_name.into(),
            invited_by,
            addrs: own_addrs.to_vec(),
        }];
        for c in all_computers(&tx)?
            .into_iter()
            .filter(|c| c.state == State::Accepted && c.key != key)
        {
            computers.push(Known {
                key: c.key,
                name: c.name.ok_or(READ)?,
                invited_by: c.invited_by,
                addrs: c.addrs,
            });
        }
        let removed = {
            let mut stmt = tx
                .prepare("SELECT key FROM tombstones ORDER BY key")
                .map_err(|_| READ)?;
            stmt.query_map([], |r| r.get::<_, String>(0))
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .map_err(|_| READ)?
        };
        if computers.len() > COMPUTERS_MAX || removed.len() > REMOVED_MAX {
            return Err("the fleet is too large to join");
        }
        tx.commit().map_err(|_| WRITE)?;
        Ok((
            JoinAnswer {
                fleet,
                computers,
                removed,
            },
            introduction,
        ))
    }

    pub fn joined(
        &mut self,
        node: &iroh::EndpointId,
        code: &JoinCode,
        answer: &JoinAnswer,
        now: f64,
    ) -> Result<(), &'static str> {
        const INVALID: &str = "the join answer is not valid";
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        if load_fleet(&tx)?.is_some() {
            return Err("already in a fleet");
        }
        fleet::accept(None, &answer.fleet).map_err(|_| INVALID)?;
        if answer.fleet.body.id != code.fleet
            || answer.computers.len() > COMPUTERS_MAX
            || answer.removed.len() > REMOVED_MAX
        {
            return Err(INVALID);
        }
        let own = node.to_string();
        let inviter = code.inviter.to_string();
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        for known in &answer.computers {
            if !known.valid()
                || known.key == own
                || !keys.insert(known.key.as_str())
                || !names.insert(known.name.as_str())
            {
                return Err(INVALID);
            }
        }
        if !keys.contains(inviter.as_str()) {
            return Err(INVALID);
        }
        for key in &answer.removed {
            if !wire::hex64(key) || key == &own || keys.contains(key.as_str()) {
                return Err(INVALID);
            }
        }
        tx.execute(
            "INSERT INTO fleet VALUES (1, ?1)",
            [serde_json::to_string(&answer.fleet).map_err(|_| WRITE)?],
        )
        .map_err(|_| WRITE)?;
        for known in &answer.computers {
            insert_computer(&tx, known, State::Accepted, now)?;
        }
        for key in &answer.removed {
            store_tombstone(&tx, key, None, &inviter, now)?;
        }
        tx.execute("INSERT INTO local VALUES (1, 1, ?1)", [inviter])
            .map_err(|_| WRITE)?;
        tx.commit().map_err(|_| WRITE)
    }

    pub fn apply(
        &mut self,
        node: &iroh::EndpointId,
        own_name: &str,
        record: &SignedRecord,
        now: f64,
    ) -> Result<Applied, &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        record.verify()?;
        let fleet = load_fleet(&tx)?.ok_or("not in a fleet")?;
        if record.fleet() != fleet.body.id {
            return Err("the record is for another fleet");
        }
        if record.signer() == node.to_string() {
            return Err("the record is this computer's own");
        }
        let counter = i64::try_from(record.counter()).map_err(|_| WRITE)?;
        let already: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE signer = ?1 AND counter = ?2)",
                params![record.signer(), counter],
                |r| r.get(0),
            )
            .map_err(|_| READ)?;
        if already {
            return Err("the record was already seen");
        }
        let (applied, kind) = match &record.body {
            RecordBody::Introduction {
                inviter,
                newcomer,
                name,
                addrs,
                ..
            } => (
                apply_introduction(&tx, node, own_name, inviter, newcomer, name, addrs, now)?,
                "introduction",
            ),
            RecordBody::Removal {
                signer, subject, ..
            } => (
                apply_removal(&tx, node, &fleet.body.founder, signer, subject, now)?,
                "removal",
            ),
        };
        if applied != Applied::LeftFleet {
            seen(&tx, record.signer(), record.counter(), kind, now)?;
        }
        tx.commit().map_err(|_| WRITE)?;
        Ok(applied)
    }

    pub fn accept(&mut self, target: &str) -> Result<Computer, &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        load_fleet(&tx)?.ok_or("not in a fleet")?;
        let mut computer = resolve(&tx, target)?;
        tx.execute(
            "UPDATE computers SET state = 'accepted' WHERE key = ?1",
            [&computer.key],
        )
        .map_err(|_| WRITE)?;
        computer.state = State::Accepted;
        tx.commit().map_err(|_| WRITE)?;
        Ok(computer)
    }
    pub fn block(&mut self, target: &str) -> Result<Computer, &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        load_fleet(&tx)?.ok_or("not in a fleet")?;
        let mut computer = resolve(&tx, target)?;
        tx.execute(
            "UPDATE computers SET state = 'blocked', trusted = 0 WHERE key = ?1",
            [&computer.key],
        )
        .map_err(|_| WRITE)?;
        computer.state = State::Blocked;
        computer.trusted = false;
        tx.commit().map_err(|_| WRITE)?;
        Ok(computer)
    }
    pub fn trust(&mut self, target: &str) -> Result<Computer, &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        load_fleet(&tx)?.ok_or("not in a fleet")?;
        let mut computer = resolve(&tx, target)?;
        if computer.state != State::Accepted {
            return Err("only an accepted computer can be trusted");
        }
        tx.execute(
            "UPDATE computers SET trusted = 1 WHERE key = ?1",
            [&computer.key],
        )
        .map_err(|_| WRITE)?;
        computer.trusted = true;
        tx.commit().map_err(|_| WRITE)?;
        Ok(computer)
    }
    pub fn remove(
        &mut self,
        node: &iroh::SecretKey,
        target: &str,
        now: f64,
    ) -> Result<(SignedRecord, iroh::EndpointId), &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let fleet = load_fleet(&tx)?.ok_or("not in a fleet")?;
        let computer = resolve(&tx, target)?;
        let own = node.public().to_string();
        if fleet.body.founder != own && computer.invited_by.as_deref() != Some(&own) {
            return Err(REMOVE_AUTH);
        }
        let key = fleet::public_key(&computer.key).ok_or(READ)?;
        delete_computer(&tx, &computer.key)?;
        store_tombstone(&tx, &computer.key, computer.name.as_deref(), &own, now)?;
        let counter = take_counter(&tx)?;
        seen(&tx, &own, counter, "removal", now)?;
        let record = membership::sign(
            RecordBody::Removal {
                fleet: fleet.body.id,
                signer: own,
                counter,
                subject: computer.key,
            },
            node,
        );
        tx.commit().map_err(|_| WRITE)?;
        Ok((record, key))
    }
    pub fn leave(
        &mut self,
        node: &iroh::SecretKey,
        now: f64,
    ) -> Result<(SignedRecord, Vec<Recipient>), &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        let fleet = load_fleet(&tx)?.ok_or("not in a fleet")?;
        let counter = take_counter(&tx)?;
        let own = node.public();
        seen(&tx, &own.to_string(), counter, "removal", now)?;
        let record = membership::sign(
            RecordBody::Removal {
                fleet: fleet.body.id,
                signer: own.to_string(),
                counter,
                subject: own.to_string(),
            },
            node,
        );
        let recipients = recipients(&tx, &own)?;
        tx.commit().map_err(|_| WRITE)?;
        Ok((record, recipients))
    }

    pub fn forget(&mut self) -> Result<(), &'static str> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WRITE)?;
        load_fleet(&tx)?.ok_or("not in a fleet")?;
        clear_fleet(&tx)?;
        tx.commit().map_err(|_| WRITE)
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

    pub fn revoke_code(&mut self, id: &str, now: f64) -> Result<bool, &'static str> {
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
        drop_expired(&tx, now)?;
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
        let row = consume(&tx, code, now)?;
        tx.commit().map_err(|_| WRITE)?;
        Ok(row)
    }
}

fn load_fleet(conn: &Connection) -> Result<Option<FleetRecord>, &'static str> {
    let text: Option<String> = conn
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

fn computer_row(row: &Row<'_>) -> rusqlite::Result<Computer> {
    let state: String = row.get(2)?;
    let state = match state.as_str() {
        "accepted" => State::Accepted,
        "pending" => State::Pending,
        "blocked" => State::Blocked,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let text: String = row.get(5)?;
    let addrs = serde_json::from_str(&text).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Computer {
        key: row.get(0)?,
        name: row.get(1)?,
        state,
        invited_by: row.get(3)?,
        trusted: row.get(4)?,
        addrs,
    })
}
fn all_computers(conn: &Connection) -> Result<Vec<Computer>, &'static str> {
    let mut stmt = conn
        .prepare("SELECT key, name, state, invited_by, trusted, addrs FROM computers ORDER BY name")
        .map_err(|_| READ)?;
    stmt.query_map([], computer_row)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|_| READ)
}
fn find_computer(conn: &Connection, key: &str) -> Result<Option<Computer>, &'static str> {
    conn.query_row(
        "SELECT key, name, state, invited_by, trusted, addrs FROM computers WHERE key = ?1",
        [key],
        computer_row,
    )
    .optional()
    .map_err(|_| READ)
}
fn tombstoned(conn: &Connection, key: &str) -> Result<bool, &'static str> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tombstones WHERE key = ?1)",
        [key],
        |r| r.get(0),
    )
    .map_err(|_| READ)
}
fn recipients(
    conn: &Connection,
    except: &iroh::EndpointId,
) -> Result<Vec<Recipient>, &'static str> {
    all_computers(conn)?
        .into_iter()
        .filter(|c| {
            c.state == State::Accepted && !c.addrs.is_empty() && c.key != except.to_string()
        })
        .map(|c| Ok((fleet::public_key(&c.key).ok_or(READ)?, c.addrs)))
        .collect()
}

fn consume(tx: &Transaction<'_>, code: &JoinCode, now: f64) -> Result<CodeRow, &'static str> {
    let hash = code.hash();
    let mut matched = None;
    {
        let mut stmt = tx
            .prepare(
                "SELECT id, name, created, expires_at, uses, hash FROM codes WHERE expires_at > ?1",
            )
            .map_err(|_| READ)?;
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
    Ok(row)
}
#[allow(clippy::too_many_arguments)]
fn apply_introduction(
    tx: &Transaction<'_>,
    node: &iroh::EndpointId,
    own_name: &str,
    inviter: &str,
    newcomer: &str,
    name: &str,
    addrs: &[SocketAddr],
    now: f64,
) -> Result<Applied, &'static str> {
    let inviter_row = find_computer(tx, inviter)?
        .filter(|c| c.state == State::Accepted)
        .ok_or("the inviter is not accepted here")?;
    if introductions(tx, inviter, now)? >= INTRODUCTIONS_PER_DAY {
        return Err("too many introductions from that computer today");
    }
    if newcomer == node.to_string()
        || tombstoned(tx, newcomer)?
        || find_computer(tx, newcomer)?.is_some()
    {
        return Err("the computer is already known");
    }
    let held: Option<String> = tx.query_row("SELECT signer FROM held WHERE subject = ?1 AND (signer = ?2 OR signer = ?1) ORDER BY signer LIMIT 1", params![newcomer, inviter], |r| r.get(0)).optional().map_err(|_| READ)?;
    if let Some(signer) = held {
        delete_computer(tx, newcomer)?;
        store_tombstone(tx, newcomer, Some(name), &signer, now)?;
        return Ok(Applied::Removed(fleet::public_key(newcomer).ok_or(READ)?));
    }
    if name == own_name || name_taken(tx, name, newcomer)? {
        return Err("the name is taken here");
    }
    let pending = !inviter_row.trusted;
    insert_computer(
        tx,
        &Known {
            key: newcomer.into(),
            name: name.into(),
            invited_by: Some(inviter.into()),
            addrs: addrs.to_vec(),
        },
        if pending {
            State::Pending
        } else {
            State::Accepted
        },
        now,
    )?;
    Ok(Applied::Added { pending })
}
const REMOVE_AUTH: &str = "only its inviter or the founding member removes a computer";
fn apply_removal(
    tx: &Transaction<'_>,
    node: &iroh::EndpointId,
    founder: &str,
    signer: &str,
    subject: &str,
    now: f64,
) -> Result<Applied, &'static str> {
    if tombstoned(tx, subject)? {
        return Err("the computer was already removed");
    }
    let founder_ok = signer == founder && !tombstoned(tx, founder)?;
    let leaving = signer == subject;
    if !leaving
        && !founder_ok
        && !find_computer(tx, signer)?.is_some_and(|c| c.state == State::Accepted)
    {
        return Err("the signer is not accepted here");
    }
    if !leaving && subject == node.to_string() {
        let inviter: Option<String> = tx
            .query_row("SELECT invited_by FROM local WHERE one = 1", [], |r| {
                r.get(0)
            })
            .map_err(|_| READ)?;
        if !founder_ok && inviter.as_deref() != Some(signer) {
            return Err(REMOVE_AUTH);
        }
        clear_fleet(tx)?;
        return Ok(Applied::LeftFleet);
    }
    if let Some(computer) = find_computer(tx, subject)? {
        if !leaving && !founder_ok && computer.invited_by.as_deref() != Some(signer) {
            return Err(REMOVE_AUTH);
        }
        delete_computer(tx, subject)?;
        store_tombstone(tx, subject, computer.name.as_deref(), signer, now)?;
        return Ok(Applied::Removed(fleet::public_key(subject).ok_or(READ)?));
    }
    if !leaving && founder_ok {
        store_tombstone(tx, subject, None, signer, now)?;
        return Ok(Applied::Removed(fleet::public_key(subject).ok_or(READ)?));
    }
    let from_signer: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM held WHERE signer = ?1",
            [signer],
            |r| r.get(0),
        )
        .map_err(|_| READ)?;
    if from_signer >= HELD_PER_SIGNER {
        return Err("too many held removals from that computer");
    }
    let total: i64 = tx
        .query_row("SELECT COUNT(*) FROM held", [], |r| r.get(0))
        .map_err(|_| READ)?;
    if total >= HELD_MAX {
        return Err("too many held removals");
    }
    tx.execute(
        "INSERT INTO held VALUES (?1, ?2, ?3)",
        params![signer, subject, now],
    )
    .map_err(|_| WRITE)?;
    Ok(Applied::Held)
}
fn delete_computer(tx: &Transaction<'_>, key: &str) -> Result<(), &'static str> {
    tx.execute("DELETE FROM computers WHERE key = ?1", [key])
        .map_err(|_| WRITE)?;
    Ok(())
}
fn clear_fleet(tx: &Transaction<'_>) -> Result<(), &'static str> {
    tx.execute_batch("DELETE FROM local; DELETE FROM computers; DELETE FROM tombstones; DELETE FROM held; DELETE FROM records; DELETE FROM codes; DELETE FROM fleet;").map_err(|_| WRITE)
}

fn resolve(conn: &Connection, target: &str) -> Result<Computer, &'static str> {
    if target.len() == 64 {
        fleet::public_key(target).ok_or("not a computer")?;
        if let Some(computer) = find_computer(conn, target)? {
            return Ok(computer);
        }
        return Err(if tombstoned(conn, target)? {
            "the computer was removed"
        } else {
            "no such computer"
        });
    }
    if !wire::member_name(target) {
        return Err("not a computer");
    }
    conn.query_row(
        "SELECT key, name, state, invited_by, trusted, addrs FROM computers WHERE name = ?1",
        [target],
        computer_row,
    )
    .optional()
    .map_err(|_| READ)?
    .ok_or("no such computer")
}

fn name_taken(conn: &Connection, name: &str, key: &str) -> Result<bool, &'static str> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM computers WHERE name = ?1 AND key != ?2)",
        params![name, key],
        |r| r.get(0),
    )
    .map_err(|_| READ)
}
fn introductions(conn: &Connection, signer: &str, now: f64) -> Result<i64, &'static str> {
    conn.query_row(
        "SELECT COUNT(*) FROM records WHERE signer = ?1 AND kind = 'introduction' AND at > ?2",
        params![signer, now - DAY],
        |r| r.get(0),
    )
    .map_err(|_| READ)
}
fn insert_computer(
    conn: &Connection,
    known: &Known,
    state: State,
    now: f64,
) -> Result<(), &'static str> {
    conn.execute(
        "INSERT INTO computers VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
        params![
            known.key,
            known.name,
            state.as_str(),
            known.invited_by,
            serde_json::to_string(&known.addrs).map_err(|_| WRITE)?,
            now
        ],
    )
    .map_err(|_| WRITE)?;
    conn.execute("DELETE FROM held WHERE subject = ?1", [&known.key])
        .map_err(|_| WRITE)?;
    Ok(())
}
fn store_tombstone(
    tx: &Transaction<'_>,
    key: &str,
    name: Option<&str>,
    signer: &str,
    now: f64,
) -> Result<(), &'static str> {
    tx.execute(
        "INSERT OR IGNORE INTO tombstones VALUES (?1, ?2, ?3, ?4)",
        params![key, name, signer, now],
    )
    .map_err(|_| WRITE)?;
    tx.execute("DELETE FROM held WHERE subject = ?1", [key])
        .map_err(|_| WRITE)?;
    Ok(())
}
fn take_counter(tx: &Transaction<'_>) -> Result<u64, &'static str> {
    let counter: i64 = tx
        .query_row("SELECT next_counter FROM local WHERE one = 1", [], |r| {
            r.get(0)
        })
        .map_err(|_| READ)?;
    let next = counter.checked_add(1).ok_or(WRITE)?;
    tx.execute("UPDATE local SET next_counter = ?1 WHERE one = 1", [next])
        .map_err(|_| WRITE)?;
    u64::try_from(counter).map_err(|_| READ)
}
fn seen(
    tx: &Transaction<'_>,
    signer: &str,
    counter: u64,
    kind: &str,
    now: f64,
) -> Result<(), &'static str> {
    tx.execute(
        "INSERT INTO records VALUES (?1, ?2, ?3, ?4)",
        params![
            signer,
            i64::try_from(counter).map_err(|_| WRITE)?,
            kind,
            now
        ],
    )
    .map_err(|_| WRITE)?;
    Ok(())
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
            tx.execute_batch(SCHEMA_1).map_err(|_| WRITE)?;
            tx.execute_batch(SCHEMA_2).map_err(|_| WRITE)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|_| WRITE)?;
            tx.commit().map_err(|_| WRITE)
        }
        1 => {
            tx.execute_batch(SCHEMA_2).map_err(|_| WRITE)?;
            tx.execute(
                "INSERT INTO local SELECT 1, 1, NULL WHERE EXISTS (SELECT 1 FROM fleet)",
                [],
            )
            .map_err(|_| WRITE)?;
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
