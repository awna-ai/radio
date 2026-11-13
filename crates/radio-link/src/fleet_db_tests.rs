use super::*;
use crate::admit::Admission;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "radio-fleet-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn path(&self) -> PathBuf {
        self.0.join(DB_FILE)
    }
    fn link(&self) -> PathBuf {
        self.0.join("link")
    }
    fn open(&self) -> FleetDb {
        FleetDb::open(&self.path()).unwrap()
    }
    fn found(&self) -> (FleetDb, iroh::SecretKey) {
        let mut db = self.open();
        let node = iroh::SecretKey::generate();
        db.found(&node, "garden", &self.link()).unwrap();
        (db, node)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}
fn mint(db: &mut FleetDb, node: &iroh::SecretKey, now: f64) -> (JoinCode, CodeRow) {
    db.create_code(node, None, None, Hints::default(), now)
        .unwrap()
}

#[test]
fn opens_private_schema_and_reopens_data() {
    let temp = Temp::new();
    let (db, _) = temp.found();
    assert_eq!(mode(&temp.0), 0o700);
    assert_eq!(mode(&temp.path()), 0o600);
    assert_eq!(
        db.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let record = db.fleet().unwrap();
    drop(db);
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(temp.open().fleet().unwrap(), record);
    assert_eq!(mode(&temp.path()), 0o600);
    assert_eq!(mode(&temp.0), 0o700);
}
#[test]
fn refuses_newer_and_negative_schema_versions() {
    for version in [3, -1] {
        let temp = Temp::new();
        let db = temp.open();
        db.conn
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(db);
        assert_eq!(
            FleetDb::open(&temp.path()).err(),
            Some(if version == 3 {
                "fleet.db is newer than this build"
            } else {
                "fleet.db could not be read"
            })
        );
    }
}
#[test]
fn refuses_symlink_file_and_folder() {
    let temp = Temp::new();
    fs::create_dir_all(&temp.0).unwrap();
    fs::write(temp.0.join("target"), []).unwrap();
    symlink(temp.0.join("target"), temp.path()).unwrap();
    assert_eq!(
        FleetDb::open(&temp.path()).err(),
        Some("fleet.db could not be opened")
    );
    fs::remove_file(temp.path()).unwrap();
    let alias = temp.0.join("alias");
    symlink(&temp.0, &alias).unwrap();
    assert_eq!(
        FleetDb::open(&alias.join(DB_FILE)).err(),
        Some("the fleet folder is not usable")
    );
}
#[test]
fn founding_writes_one_private_recovery_key() {
    let temp = Temp::new();
    let mut db = temp.open();
    let node = iroh::SecretKey::generate();
    let (record, path) = db.found(&node, "garden", &temp.link()).unwrap();
    assert_eq!(path, temp.link().join(RECOVERY_FILE));
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&temp.link()), 0o700);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut bytes = [0u8; 128];
        // SAFETY: both strings are NUL-terminated and bytes spans its advertised length.
        let length = unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"com.apple.metadata:com_apple_backup_excludeItem".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        assert!(length > 0, "recovery key must be excluded from backups");
    }
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 65);
    assert_eq!(bytes[64], b'\n');
    let hex = std::str::from_utf8(&bytes[..64]).unwrap();
    assert!(crate::wire::hex64(hex));
    let mut secret = [0; 32];
    for (i, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    assert_eq!(
        iroh::SecretKey::from_bytes(&secret).public().to_string(),
        record.body.recovery
    );
    assert_eq!(db.fleet().unwrap(), Some(record));
    assert_eq!(
        db.found(&node, "orchard", &temp.link()).err(),
        Some("already in a fleet")
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}
#[test]
fn refuses_waiting_recovery_file_without_storing_fleet() {
    let temp = Temp::new();
    let mut db = temp.open();
    crate::key::private_folder(&temp.link()).unwrap();
    let path = temp.link().join(RECOVERY_FILE);
    fs::write(&path, b"waiting").unwrap();
    assert_eq!(
        db.found(&iroh::SecretKey::generate(), "garden", &temp.link())
            .err(),
        Some("a recovery key file is waiting to be moved")
    );
    assert_eq!(db.fleet().unwrap(), None);
    assert_eq!(fs::read(path).unwrap(), b"waiting");
}
#[test]
fn failed_fleet_insert_removes_only_new_recovery_file() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.conn.execute_batch("CREATE TRIGGER refuse_fleet BEFORE INSERT ON fleet BEGIN SELECT RAISE(ABORT, 'refuse'); END;").unwrap();
    assert_eq!(
        db.found(&iroh::SecretKey::generate(), "garden", &temp.link())
            .err(),
        Some("fleet.db could not be written")
    );
    assert!(!temp.link().join(RECOVERY_FILE).exists());
    assert_eq!(db.fleet().unwrap(), None);
}
#[test]
fn damaged_stored_record_is_refused() {
    let temp = Temp::new();
    let (db, _) = temp.found();
    db.conn
        .execute("UPDATE fleet SET record = '{}'", [])
        .unwrap();
    assert_eq!(db.fleet(), Err("fleet.db holds a damaged fleet record"));
}
#[test]
fn validates_code_names_and_lifetimes() {
    let temp = Temp::new();
    let node = iroh::SecretKey::generate();
    let mut db = temp.open();
    assert!(!db.join_open(10.0).unwrap());
    let unowned = JoinCode::new("00".repeat(32), node.public(), Hints::default()).unwrap();
    assert_eq!(
        db.consume_code(&unowned, &node.public(), 10.0),
        Err("not a live join code")
    );
    assert_eq!(
        db.create_code(&node, None, None, Hints::default(), 10.0)
            .err(),
        Some("not in a fleet")
    );
    db.found(&node, "garden", &temp.link()).unwrap();
    let (_, row) = mint(&mut db, &node, 10.0);
    assert_eq!(row.created, 10.0);
    assert_eq!(row.expires_at, 1810.0);
    assert_eq!(row.uses, 1);
    for ttl in [59, 86401] {
        assert_eq!(
            db.create_code(&node, None, Some(ttl), Hints::default(), 10.0)
                .err(),
            Some("not a code lifetime")
        );
    }
    for ttl in [60, 86400] {
        assert_eq!(
            db.create_code(&node, None, Some(ttl), Hints::default(), 10.0)
                .unwrap()
                .1
                .expires_at,
            10.0 + ttl as f64
        );
    }
    assert_eq!(
        db.create_code(&node, Some("Garden"), None, Hints::default(), 10.0)
            .err(),
        Some("not a code name")
    );
    let (_, named) = db
        .create_code(&node, Some("orchard"), None, Hints::default(), 11.0)
        .unwrap();
    assert_eq!(named.name.as_deref(), Some("orchard"));
    assert!(db.codes(11.0).unwrap().contains(&named));
}
#[test]
fn keeps_only_hash_in_database_and_journals() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let (code, row) = mint(&mut db, &node, 10.0);
    let hash: Vec<u8> = db
        .conn
        .query_row("SELECT hash FROM codes WHERE id = ?1", [&row.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(hash, Sha256::digest(code.secret).as_slice());
    let encoded = data_encoding::BASE64URL_NOPAD.encode(&code.secret);
    for suffix in ["", "-wal", "-shm"] {
        let path = temp.0.join(format!("{DB_FILE}{suffix}"));
        assert!(path.exists());
        let bytes = fs::read(path).unwrap();
        assert!(!bytes.windows(32).any(|w| w == code.secret));
        assert!(
            !bytes
                .windows(encoded.len())
                .any(|w| w == encoded.as_bytes())
        );
    }
}
#[test]
fn caps_live_codes_and_expiry_frees_capacity() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    db.create_code(&node, None, Some(60), Hints::default(), 10.0)
        .unwrap();
    for _ in 1..CODES_MAX {
        mint(&mut db, &node, 10.0);
    }
    assert_eq!(
        db.create_code(&node, None, None, Hints::default(), 10.0)
            .err(),
        Some("too many open join codes")
    );
    mint(&mut db, &node, 70.0);
    assert_eq!(db.codes(70.0).unwrap().len(), CODES_MAX);
}
#[test]
fn lists_in_creation_and_id_order_and_drops_expired() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let (_, late) = mint(&mut db, &node, 20.0);
    let (_, early) = mint(&mut db, &node, 10.0);
    let (_, tied) = mint(&mut db, &node, 20.0);
    let rows = db.codes(10.0).unwrap();
    assert_eq!(rows[0], early);
    assert_eq!(rows[1].id, late.id.clone().min(tied.id.clone()));
    assert_eq!(rows[2].id, late.id.max(tied.id));
    assert!(db.codes(1820.0).unwrap().is_empty());
    assert_eq!(
        db.conn
            .query_row("SELECT COUNT(*) FROM codes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn revokes_valid_ids_and_refuses_bad_ids() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    for id in ["", "a", "FFFFFFFFFFFFFFFF", "0123456789abcdeg"] {
        assert_eq!(db.revoke_code(id, 10.0), Err("not a code id"));
    }
    assert!(!db.revoke_code("0000000000000000", 10.0).unwrap());
    let (code, row) = mint(&mut db, &node, 10.0);
    assert!(db.revoke_code(&row.id, 10.0).unwrap());
    assert!(!db.revoke_code(&row.id, 10.0).unwrap());
    assert_eq!(
        db.consume_code(&code, &node.public(), 10.0),
        Err("not a live join code")
    );
}
#[test]
fn consumes_once_and_refuses_expired_wrong_fleet_inviter_and_secret() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let (code, row) = mint(&mut db, &node, 10.0);
    let mut wrong = code.clone();
    wrong.fleet = "00".repeat(32);
    assert_eq!(
        db.consume_code(&wrong, &node.public(), 10.0),
        Err("not a live join code")
    );
    wrong = code.clone();
    wrong.inviter = iroh::SecretKey::generate().public();
    assert_eq!(
        db.consume_code(&wrong, &node.public(), 10.0),
        Err("not a live join code")
    );
    wrong = code.clone();
    wrong.secret[0] ^= 1;
    assert_eq!(
        db.consume_code(&wrong, &node.public(), 10.0),
        Err("not a live join code")
    );
    assert_eq!(db.consume_code(&code, &node.public(), 10.0).unwrap(), row);
    assert_eq!(
        db.consume_code(&code, &node.public(), 10.0),
        Err("not a live join code")
    );
    let (expired, _) = mint(&mut db, &node, 10.0);
    assert_eq!(
        db.consume_code(&expired, &node.public(), 1810.0),
        Err("not a live join code")
    );
}
#[test]
fn join_open_tracks_expiry_consumption_and_revocation() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    assert!(!db.join_open(10.0).unwrap());
    let (code, _) = mint(&mut db, &node, 10.0);
    assert!(db.join_open(10.0).unwrap());
    assert!(!db.join_open(1810.0).unwrap());
    db.consume_code(&code, &node.public(), 10.0).unwrap();
    assert!(!db.join_open(10.0).unwrap());
    let (_, row) = mint(&mut db, &node, 10.0);
    db.revoke_code(&row.id, 10.0).unwrap();
    assert!(!db.join_open(10.0).unwrap());
}
#[test]
fn multiple_use_column_decrements_without_reaching_zero() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let (code, row) = mint(&mut db, &node, 10.0);
    db.conn
        .execute("UPDATE codes SET uses = 2 WHERE id = ?1", [&row.id])
        .unwrap();
    assert_eq!(
        db.consume_code(&code, &node.public(), 10.0).unwrap().uses,
        2
    );
    assert_eq!(
        db.consume_code(&code, &node.public(), 10.0).unwrap().uses,
        1
    );
    assert!(!db.join_open(10.0).unwrap());
}
#[test]
fn two_consumers_race_fifty_codes_with_exactly_one_success_each() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    for _ in 0..50 {
        let (code, _) = mint(&mut db, &node, 10.0);
        let second = temp.open();
        let barrier = Arc::new(Barrier::new(2));
        let other_barrier = barrier.clone();
        let other_code = code.clone();
        let own = node.public();
        let worker = thread::spawn(move || {
            let mut db = second;
            other_barrier.wait();
            db.consume_code(&other_code, &own, 10.0)
        });
        barrier.wait();
        let first = db.consume_code(&code, &own, 10.0);
        let second = worker.join().unwrap();
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(first.err().or(second.err()), Some("not a live join code"));
    }
}

#[test]
fn revoking_an_unswept_expired_code_returns_false() {
    for elapsed in [0.0, 1.0] {
        let temp = Temp::new();
        let (mut db, node) = temp.found();
        let (_, row) = mint(&mut db, &node, 10.0);
        assert!(!db.revoke_code(&row.id, row.expires_at + elapsed).unwrap());
    }
}

#[test]
fn schema_two_has_every_table_and_found_local() {
    let temp = Temp::new();
    let (db, _) = temp.found();
    assert_eq!(
        db.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    for table in [
        "fleet",
        "codes",
        "local",
        "computers",
        "tombstones",
        "held",
        "records",
    ] {
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    assert_eq!(
        db.conn
            .query_row("SELECT one, next_counter, invited_by FROM local", [], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap(),
        (1, 1, None)
    );
}
#[test]
fn migrates_version_one_with_and_without_fleet() {
    for populated in [false, true] {
        let temp = Temp::new();
        key::private_folder(&temp.0).unwrap();
        let conn = Connection::open(temp.path()).unwrap();
        conn.execute_batch(SCHEMA_1).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let node = iroh::SecretKey::generate();
        let record = fleet::found(&node, "garden").unwrap().0;
        if populated {
            conn.execute(
                "INSERT INTO fleet VALUES (1, ?1)",
                [serde_json::to_string(&record).unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO codes VALUES ('0123456789abcdef', ?1, NULL, 1.0, 100.0, 1)",
                [vec![7u8; 32]],
            )
            .unwrap();
        }
        drop(conn);
        let db = temp.open();
        assert_eq!(
            db.conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM local", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            i64::from(populated)
        );
        if populated {
            assert_eq!(db.fleet().unwrap(), Some(record));
            assert!(db.join_open(10.0).unwrap());
            assert_eq!(
                db.conn
                    .query_row("SELECT next_counter, invited_by FROM local", [], |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?
                    )))
                    .unwrap(),
                (1, None)
            );
        }
    }
}

fn computer(
    db: &FleetDb,
    key: &iroh::EndpointId,
    name: &str,
    state: State,
    inviter: Option<&iroh::EndpointId>,
    addrs: &[SocketAddr],
) {
    db.conn
        .execute(
            "INSERT INTO computers VALUES (?1, ?2, ?3, ?4, 0, ?5, 10.0)",
            params![
                key.to_string(),
                name,
                state.as_str(),
                inviter.map(|k| k.to_string()),
                serde_json::to_string(addrs).unwrap()
            ],
        )
        .unwrap();
}
fn tombstone(db: &FleetDb, key: &iroh::EndpointId, signer: &iroh::EndpointId) {
    db.conn
        .execute(
            "INSERT INTO tombstones VALUES (?1, NULL, ?2, 10.0)",
            params![key.to_string(), signer.to_string()],
        )
        .unwrap();
}
fn addr() -> Vec<SocketAddr> {
    vec!["127.0.0.1:7000".parse().unwrap()]
}
#[test]
fn readings_standing_list_and_recipients() {
    let temp = Temp::new();
    let (db, node) = temp.found();
    let keys: Vec<_> = (0..6)
        .map(|_| iroh::SecretKey::generate().public())
        .collect();
    computer(
        &db,
        &keys[0],
        "orchard",
        State::Accepted,
        Some(&node.public()),
        &addr(),
    );
    computer(&db, &keys[1], "garden", State::Pending, None, &addr());
    computer(&db, &keys[2], "meadow", State::Blocked, None, &addr());
    computer(&db, &keys[3], "harbor", State::Accepted, None, &[]);
    tombstone(&db, &keys[4], &node.public());
    tombstone(&db, &keys[5], &keys[5]);
    for (key, expected) in keys.iter().zip([
        Standing::Accepted,
        Standing::Unknown,
        Standing::Refused,
        Standing::Accepted,
        Standing::Refused,
        Standing::Refused,
    ]) {
        assert_eq!(db.standing(key), Ok(expected));
    }
    assert_eq!(db.standing(&node.public()), Ok(Standing::Unknown));
    let list = db.computers().unwrap();
    assert_eq!(
        list[..4]
            .iter()
            .map(|c| c.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["garden", "harbor", "meadow", "orchard"]
    );
    assert_eq!(
        list[..4].iter().map(|c| c.state).collect::<Vec<_>>(),
        [
            State::Pending,
            State::Accepted,
            State::Blocked,
            State::Accepted
        ]
    );
    assert!(list[4].key < list[5].key);
    for c in &list[4..] {
        assert_eq!(
            c.state,
            if c.key == keys[5].to_string() {
                State::Left
            } else {
                State::Removed
            }
        );
        assert!(!c.trusted);
        assert!(c.invited_by.is_none() && c.addrs.is_empty());
    }
    assert_eq!(
        db.recipients(&node.public()).unwrap(),
        vec![(keys[0], addr())]
    );
    assert!(db.recipients(&keys[0]).unwrap().is_empty());
}
#[test]
fn readings_admission_tracks_db_and_fails_closed() {
    let temp = Temp::new();
    let (db, node) = temp.found();
    let peer = iroh::SecretKey::generate().public();
    computer(&db, &peer, "orchard", State::Accepted, None, &addr());
    let fleet = db.fleet().unwrap().unwrap().body.id;
    let book = std::sync::Mutex::new(db);
    assert_eq!(Admission::standing(&book, &peer), Standing::Accepted);
    assert!(!Admission::join_open(&book));
    assert_eq!(Admission::fleet(&book), Some(fleet));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    mint(&mut book.lock().unwrap(), &node, now);
    assert!(Admission::join_open(&book));
    book.lock()
        .unwrap()
        .conn
        .execute_batch("DROP TABLE computers")
        .unwrap();
    assert_eq!(Admission::standing(&book, &peer), Standing::Refused);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lock = book.lock().unwrap();
        panic!("poison");
    }));
    assert_eq!(Admission::standing(&book, &peer), Standing::Refused);
    assert!(!Admission::join_open(&book));
    assert_eq!(Admission::fleet(&book), None);
}

fn admit_new(
    db: &mut FleetDb,
    node: &iroh::SecretKey,
    newcomer: &iroh::SecretKey,
    name: &str,
    now: f64,
) -> (JoinAnswer, Option<SignedRecord>) {
    let (code, _) = mint(db, node, now);
    db.admit(
        node,
        "lantern",
        &addr(),
        &code,
        &newcomer.public(),
        name,
        &addr(),
        &membership::consent(
            newcomer,
            &code.fleet,
            &node.public().to_string(),
            name,
            &addr(),
        ),
        now,
    )
    .unwrap()
}
fn count(db: &FleetDb, table: &str) -> i64 {
    db.conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn admitting_new_keys_signs_counters_and_answers_with_accepted_view() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let peer_keys: Vec<_> = (0..5).map(|_| iroh::SecretKey::generate()).collect();
    let peers: Vec<_> = peer_keys.iter().map(|k| k.public()).collect();
    computer(&db, &peers[0], "orchard", State::Accepted, None, &addr());
    computer(&db, &peers[1], "meadow", State::Pending, None, &addr());
    computer(&db, &peers[2], "harbor", State::Blocked, None, &addr());
    tombstone(&db, &peers[3], &node.public());
    let (code, _) = mint(&mut db, &node, 10.0);
    let (answer, record) = db
        .admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &peers[4],
            "juniper",
            &addr(),
            &membership::consent(
                &peer_keys[4],
                &code.fleet,
                &node.public().to_string(),
                "juniper",
                &addr(),
            ),
            10.0,
        )
        .unwrap();
    let record = record.unwrap();
    assert_eq!(record.verify(), Ok(()));
    assert_eq!(record.counter(), 1);
    assert_eq!(
        answer
            .computers
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["lantern", "orchard"]
    );
    assert_eq!(answer.removed, vec![peers[3].to_string()]);
    assert_eq!(answer.computers[0].addrs, addr());
    assert_eq!(db.standing(&peers[4]), Ok(Standing::Accepted));
    assert_eq!(
        find_computer(&db.conn, &peers[4].to_string())
            .unwrap()
            .unwrap()
            .invited_by,
        Some(node.public().to_string())
    );
    assert_eq!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &peers[4],
            "juniper",
            &addr(),
            &membership::consent(
                &peer_keys[4],
                &code.fleet,
                &node.public().to_string(),
                "juniper",
                &addr()
            ),
            10.0
        )
        .err(),
        Some(NOT_LIVE)
    );
    assert_eq!(
        admit_new(&mut db, &node, &iroh::SecretKey::generate(), "garden", 11.0)
            .1
            .unwrap()
            .counter(),
        2
    );
}
#[test]
fn admitting_refusals_leave_code_live() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let blocked_secret = iroh::SecretKey::generate();
    let blocked = blocked_secret.public();
    let removed_secret = iroh::SecretKey::generate();
    let removed = removed_secret.public();
    computer(&db, &blocked, "orchard", State::Blocked, None, &addr());
    tombstone(&db, &removed, &node.public());
    let newcomer_secret = iroh::SecretKey::generate();
    let newcomer = newcomer_secret.public();
    let (code, _) = mint(&mut db, &node, 10.0);
    for (key, key_secret, name, addrs, phrase) in [
        (blocked, &blocked_secret, "garden", addr(), "not let in"),
        (removed, &removed_secret, "garden", addr(), "not let in"),
        (node.public(), &node, "garden", addr(), "not let in"),
        (
            newcomer,
            &newcomer_secret,
            "Garden",
            addr(),
            "not a computer name",
        ),
        (
            newcomer,
            &newcomer_secret,
            "lantern",
            addr(),
            "the name is taken",
        ),
        (
            newcomer,
            &newcomer_secret,
            "orchard",
            addr(),
            "the name is taken",
        ),
        (
            newcomer,
            &newcomer_secret,
            "garden",
            vec![],
            "the computer's addresses are not usable",
        ),
        (
            newcomer,
            &newcomer_secret,
            "garden",
            vec![addr()[0]; 3],
            "the computer's addresses are not usable",
        ),
        (
            newcomer,
            &newcomer_secret,
            "garden",
            vec!["0.0.0.0:7000".parse().unwrap()],
            "the computer's addresses are not usable",
        ),
    ] {
        assert_eq!(
            db.admit(
                &node,
                "lantern",
                &addr(),
                &code,
                &key,
                name,
                &addrs,
                &membership::consent(
                    key_secret,
                    &code.fleet,
                    &node.public().to_string(),
                    name,
                    &addrs
                ),
                10.0
            )
            .err(),
            Some(phrase)
        );
        assert!(db.join_open(10.0).unwrap());
        assert_eq!(count(&db, "records"), 0);
    }
    for wrong in [
        JoinCode {
            fleet: "ab".repeat(32),
            ..code.clone()
        },
        JoinCode {
            inviter: newcomer,
            ..code.clone()
        },
    ] {
        assert_eq!(
            db.admit(
                &node,
                "lantern",
                &addr(),
                &wrong,
                &newcomer,
                "garden",
                &addr(),
                &membership::consent(
                    &newcomer_secret,
                    &code.fleet,
                    &node.public().to_string(),
                    "garden",
                    &addr()
                ),
                10.0
            )
            .err(),
            Some(NOT_LIVE)
        );
    }
    assert!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &newcomer,
            "garden",
            &addr(),
            &membership::consent(
                &newcomer_secret,
                &code.fleet,
                &node.public().to_string(),
                "garden",
                &addr()
            ),
            10.0
        )
        .is_ok()
    );
}
#[test]
fn admitting_rejoins_update_without_introductions() {
    for state in [State::Accepted, State::Pending] {
        let temp = Temp::new();
        let (mut db, node) = temp.found();
        let peer_secret = iroh::SecretKey::generate();
        let peer = peer_secret.public();
        let inviter = iroh::SecretKey::generate().public();
        computer(
            &db,
            &peer,
            "orchard",
            state,
            Some(&inviter),
            &["127.0.0.1:7001".parse().unwrap()],
        );
        assert!(
            admit_new(&mut db, &node, &peer_secret, "garden", 10.0)
                .1
                .is_none()
        );
        let c = find_computer(&db.conn, &peer.to_string()).unwrap().unwrap();
        assert_eq!(c.name.as_deref(), Some("garden"));
        assert_eq!(c.addrs, addr());
        assert_eq!(c.invited_by, Some(inviter.to_string()));
        assert_eq!(c.state, State::Accepted);
        assert_eq!(count(&db, "records"), 0);
    }
}
#[test]
fn admitting_daily_limit_rolls_back_consumption() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    for (n, name) in ["garden", "orchard", "meadow"].iter().enumerate() {
        admit_new(
            &mut db,
            &node,
            &iroh::SecretKey::generate(),
            name,
            10.0 + n as f64,
        );
    }
    let peer_secret = iroh::SecretKey::generate();
    let peer = peer_secret.public();
    let (code, _) = db
        .create_code(&node, None, Some(86400), Hints::default(), 13.0)
        .unwrap();
    assert_eq!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &peer,
            "harbor",
            &addr(),
            &membership::consent(
                &peer_secret,
                &code.fleet,
                &node.public().to_string(),
                "harbor",
                &addr()
            ),
            13.0
        )
        .err(),
        Some("too many introductions today")
    );
    assert!(db.join_open(13.0).unwrap());
    assert!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &peer,
            "harbor",
            &addr(),
            &membership::consent(
                &peer_secret,
                &code.fleet,
                &node.public().to_string(),
                "harbor",
                &addr()
            ),
            10.0 + DAY + 1.0
        )
        .is_ok()
    );
}

fn answer(node: &iroh::SecretKey) -> (JoinCode, JoinAnswer) {
    let fleet = fleet::found(node, "garden").unwrap().0;
    let code = JoinCode::new(fleet.body.id.clone(), node.public(), Hints::default()).unwrap();
    let answer = JoinAnswer {
        fleet,
        computers: vec![
            Known {
                key: node.public().to_string(),
                name: "lantern".into(),
                invited_by: None,
                addrs: addr(),
            },
            Known {
                key: iroh::SecretKey::generate().public().to_string(),
                name: "orchard".into(),
                invited_by: Some(node.public().to_string()),
                addrs: vec![],
            },
        ],
        removed: vec![iroh::SecretKey::generate().public().to_string()],
    };
    (code, answer)
}
#[test]
fn joining_stores_view_and_local_inviter() {
    let temp = Temp::new();
    let mut db = temp.open();
    let inviter = iroh::SecretKey::generate();
    let node = iroh::SecretKey::generate().public();
    let (code, answer) = answer(&inviter);
    db.joined(&node, &code, &answer, 10.0).unwrap();
    assert_eq!(db.fleet().unwrap(), Some(answer.fleet.clone()));
    for known in &answer.computers {
        let c = find_computer(&db.conn, &known.key).unwrap().unwrap();
        assert_eq!(c.state, State::Accepted);
        assert!(!c.trusted);
        assert_eq!(c.addrs, known.addrs);
        assert_eq!(c.invited_by, known.invited_by);
    }
    assert!(tombstoned(&db.conn, &answer.removed[0]).unwrap());
    assert_eq!(
        db.conn
            .query_row("SELECT next_counter, invited_by FROM local", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?
            )))
            .unwrap(),
        (1, inviter.public().to_string())
    );
    assert_eq!(
        db.joined(&node, &code, &answer, 10.0),
        Err("already in a fleet")
    );
}
#[test]
fn joining_invalid_answers_leave_no_state() {
    let inviter = iroh::SecretKey::generate();
    let node = iroh::SecretKey::generate().public();
    let (code, base) = answer(&inviter);
    let mut invalid = vec![];
    let mut a = base.clone();
    a.fleet.signature = "00".repeat(64);
    invalid.push(a);
    let mut a = base.clone();
    a.fleet = fleet::found(&inviter, "garden").unwrap().0;
    invalid.push(a);
    let mut a = base.clone();
    a.computers = vec![a.computers[0].clone(); COMPUTERS_MAX + 1];
    invalid.push(a);
    let mut a = base.clone();
    a.removed = vec![a.removed[0].clone(); REMOVED_MAX + 1];
    invalid.push(a);
    let mut a = base.clone();
    a.computers[1].key = node.to_string();
    invalid.push(a);
    let mut a = base.clone();
    a.computers[1].key = a.computers[0].key.clone();
    invalid.push(a);
    let mut a = base.clone();
    a.computers[1].name = a.computers[0].name.clone();
    invalid.push(a);
    let mut a = base.clone();
    a.computers.remove(0);
    invalid.push(a);
    for removed in [
        "AB".repeat(32),
        "a".repeat(63),
        node.to_string(),
        base.computers[0].key.clone(),
    ] {
        let mut a = base.clone();
        a.removed = vec![removed];
        invalid.push(a);
    }
    for (field, value) in [
        ("key", serde_json::json!("AB".repeat(32))),
        ("key", serde_json::json!("a".repeat(63))),
        ("name", serde_json::json!("Garden")),
        ("invited_by", serde_json::json!("z".repeat(64))),
        (
            "addrs",
            serde_json::json!(["127.0.0.1:7000", "127.0.0.2:7000"]),
        ),
        ("addrs", serde_json::json!(["[::ffff:127.0.0.1]:7000"])),
        ("addrs", serde_json::json!(["0.0.0.0:7000"])),
        (
            "addrs",
            serde_json::json!(["127.0.0.1:7000", "[::1]:7000", "127.0.0.2:7000"]),
        ),
    ] {
        let mut a = base.clone();
        let mut known = serde_json::to_value(&a.computers[1]).unwrap();
        known[field] = value;
        a.computers[1] = serde_json::from_value(known).unwrap();
        invalid.push(a);
    }
    for a in invalid {
        let temp = Temp::new();
        let mut db = temp.open();
        assert_eq!(
            db.joined(&node, &code, &a, 10.0),
            Err("the join answer is not valid")
        );
        assert_eq!(db.fleet().unwrap(), None);
        for table in ["computers", "local", "tombstones"] {
            assert_eq!(count(&db, table), 0);
        }
    }
}

fn intro(
    db: &FleetDb,
    inviter: &iroh::SecretKey,
    key: &iroh::SecretKey,
    name: &str,
    counter: u64,
) -> SignedRecord {
    membership::sign(
        RecordBody::Introduction {
            fleet: db.fleet().unwrap().unwrap().body.id,
            inviter: inviter.public().to_string(),
            counter,
            newcomer: key.public().to_string(),
            consent: membership::consent(
                key,
                &db.fleet().unwrap().unwrap().body.id,
                &inviter.public().to_string(),
                name,
                &addr(),
            ),
            name: name.into(),
            addrs: addr(),
        },
        inviter,
    )
}
fn removal(
    db: &FleetDb,
    signer: &iroh::SecretKey,
    subject: &iroh::EndpointId,
    counter: u64,
) -> SignedRecord {
    membership::sign(
        RecordBody::Removal {
            fleet: db.fleet().unwrap().unwrap().body.id,
            signer: signer.public().to_string(),
            counter,
            subject: subject.to_string(),
        },
        signer,
    )
}
#[test]
fn applying_introductions_respects_trust_replay_and_block() {
    for trusted in [false, true] {
        let temp = Temp::new();
        let (mut db, node) = temp.found();
        let inviter = iroh::SecretKey::generate();
        let peer_secret = iroh::SecretKey::generate();
        let peer = peer_secret.public();
        computer(
            &db,
            &inviter.public(),
            "orchard",
            State::Accepted,
            None,
            &addr(),
        );
        db.conn
            .execute("UPDATE computers SET trusted = ?1", [trusted])
            .unwrap();
        let record = intro(&db, &inviter, &peer_secret, "garden", 1);
        assert_eq!(
            db.apply(&node.public(), "lantern", &record, 10.0),
            Ok(Applied::Added { pending: !trusted })
        );
        assert_eq!(
            db.standing(&peer),
            Ok(if trusted {
                Standing::Accepted
            } else {
                Standing::Unknown
            })
        );
        let c = find_computer(&db.conn, &peer.to_string()).unwrap().unwrap();
        assert_eq!(c.invited_by, Some(inviter.public().to_string()));
        assert_eq!(c.addrs, addr());
        db.conn
            .execute(
                "UPDATE computers SET state = 'blocked' WHERE key = ?1",
                [peer.to_string()],
            )
            .unwrap();
        assert_eq!(
            db.apply(&node.public(), "lantern", &record, 11.0),
            Err("the record was already seen")
        );
        assert_eq!(db.standing(&peer), Ok(Standing::Refused));
        assert_eq!(count(&db, "records"), 1);
    }
}
#[test]
fn applying_invalid_or_unauthorized_records_drops_every_write() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    let peer_secret = iroh::SecretKey::generate();
    let record = intro(&db, &inviter, &peer_secret, "garden", 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Err("the inviter is not accepted here")
    );
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Pending,
        None,
        &addr(),
    );
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Err("the inviter is not accepted here")
    );
    db.conn
        .execute("UPDATE computers SET state = 'accepted'", [])
        .unwrap();
    let wrong_signature = membership::sign(record.body.clone(), &node);
    assert_eq!(
        db.apply(&node.public(), "lantern", &wrong_signature, 10.0),
        Err("the membership record's signature does not verify")
    );
    let own = intro(&db, &node, &peer_secret, "garden", 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &own, 10.0),
        Err("the record is this computer's own")
    );
    let mut body = record.body.clone();
    if let RecordBody::Introduction { fleet, consent, .. } = &mut body {
        *fleet = "ab".repeat(32);
        *consent = membership::consent(
            &peer_secret,
            fleet,
            &inviter.public().to_string(),
            "garden",
            &addr(),
        );
    }
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &membership::sign(body, &inviter),
            10.0
        ),
        Err("the record is for another fleet")
    );
    let taken = intro(&db, &inviter, &peer_secret, "orchard", 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &taken, 10.0),
        Err("the name is taken here")
    );
    assert_eq!(count(&db, "records"), 0);
    assert_eq!(count(&db, "computers"), 1);
}
#[test]
fn applying_known_introductions_never_stores_records() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    let removed_secret = iroh::SecretKey::generate();
    let removed = removed_secret.public();
    tombstone(&db, &removed, &node.public());
    let known_secret = iroh::SecretKey::generate();
    let known = known_secret.public();
    computer(&db, &known, "harbor", State::Blocked, None, &addr());
    for key_secret in [&node, &removed_secret, &known_secret] {
        for counter in 1..=100 {
            assert_eq!(
                db.apply(
                    &node.public(),
                    "lantern",
                    &intro(&db, &inviter, key_secret, "garden", counter),
                    10.0
                ),
                Err("the computer is already known")
            );
            assert_eq!(count(&db, "records"), 0);
        }
    }
}
#[test]
fn applying_introductions_daily_limit() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    for (n, name) in ["garden", "meadow", "harbor"].iter().enumerate() {
        let record = intro(
            &db,
            &inviter,
            &iroh::SecretKey::generate(),
            name,
            n as u64 + 1,
        );
        assert_eq!(
            db.apply(&node.public(), "lantern", &record, 10.0),
            Ok(Applied::Added { pending: true })
        );
    }
    let record = intro(&db, &inviter, &iroh::SecretKey::generate(), "juniper", 4);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 11.0),
        Err("too many introductions from that computer today")
    );
    assert_eq!(count(&db, "records"), 3);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0 + DAY + 1.0),
        Ok(Applied::Added { pending: true })
    );
}
#[test]
fn applying_tombstoned_removals_never_stores_records() {
    for leaving in [false, true] {
        let temp = Temp::new();
        let (mut db, node) = temp.found();
        let signer = iroh::SecretKey::generate();
        let subject = if leaving {
            signer.public()
        } else {
            iroh::SecretKey::generate().public()
        };
        computer(
            &db,
            &signer.public(),
            "orchard",
            State::Accepted,
            None,
            &addr(),
        );
        tombstone(&db, &subject, &node.public());
        for counter in 1..=100 {
            assert_eq!(
                db.apply(
                    &node.public(),
                    "lantern",
                    &removal(&db, &signer, &subject, counter),
                    10.0
                ),
                Err("the computer was already removed")
            );
            assert_eq!(count(&db, "records"), 0);
        }
    }
}
#[test]
fn applying_only_inviter_or_founder_removes_a_known_computer() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    let stranger = iroh::SecretKey::generate();
    let peer = iroh::SecretKey::generate().public();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    computer(
        &db,
        &stranger.public(),
        "meadow",
        State::Accepted,
        None,
        &addr(),
    );
    computer(
        &db,
        &peer,
        "garden",
        State::Accepted,
        Some(&inviter.public()),
        &addr(),
    );
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &stranger, &peer, 1),
            10.0
        ),
        Err("only its inviter or the founding member removes a computer")
    );
    assert_eq!(db.standing(&peer), Ok(Standing::Accepted));
    assert_eq!(count(&db, "records"), 0);
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &inviter, &peer, 1),
            10.0
        ),
        Ok(Applied::Removed(peer))
    );
    assert_eq!(db.standing(&peer), Ok(Standing::Refused));
    let record = removal(&db, &stranger, &peer, 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Err("the computer was already removed")
    );
    let other = iroh::SecretKey::generate().public();
    computer(
        &db,
        &other,
        "garden",
        State::Accepted,
        Some(&stranger.public()),
        &addr(),
    );
    let founder_record = removal(&db, &node, &other, 1);
    assert_eq!(
        db.apply(
            &iroh::SecretKey::generate().public(),
            "lantern",
            &founder_record,
            10.0
        ),
        Ok(Applied::Removed(other))
    );
}
#[test]
fn applying_held_removals_and_leaves_win_only_for_their_invitee() {
    for signer_kind in 0..3 {
        let temp = Temp::new();
        let (mut db, node) = temp.found();
        let inviter = iroh::SecretKey::generate();
        let newcomer = iroh::SecretKey::generate();
        let stranger = iroh::SecretKey::generate();
        computer(
            &db,
            &inviter.public(),
            "orchard",
            State::Accepted,
            None,
            &addr(),
        );
        computer(
            &db,
            &stranger.public(),
            "meadow",
            State::Accepted,
            None,
            &addr(),
        );
        let signer = match signer_kind {
            0 => &inviter,
            1 => &newcomer,
            _ => &stranger,
        };
        assert_eq!(
            db.apply(
                &node.public(),
                "lantern",
                &removal(&db, signer, &newcomer.public(), 1),
                10.0
            ),
            Ok(Applied::Held)
        );
        let record = intro(&db, &inviter, &newcomer, "garden", 2);
        assert_eq!(
            db.apply(&node.public(), "lantern", &record, 11.0),
            Ok(if signer_kind == 2 {
                Applied::Added { pending: true }
            } else {
                Applied::Removed(newcomer.public())
            })
        );
        assert_eq!(count(&db, "held"), 0);
        if signer_kind != 2 {
            let c = db
                .computers()
                .unwrap()
                .into_iter()
                .find(|c| c.key == newcomer.public().to_string())
                .unwrap();
            assert_eq!(
                c.state,
                if signer_kind == 1 {
                    State::Left
                } else {
                    State::Removed
                }
            );
            assert_eq!(c.name.as_deref(), Some("garden"));
        }
    }
}
#[test]
fn applying_founder_tombstones_unknown_and_unknown_signer_is_refused() {
    let temp = Temp::new();
    let (mut db, founder) = temp.found();
    let node = iroh::SecretKey::generate().public();
    let peer = iroh::SecretKey::generate().public();
    let stranger = iroh::SecretKey::generate();
    assert_eq!(
        db.apply(&node, "lantern", &removal(&db, &stranger, &peer, 1), 10.0),
        Err("the signer is not accepted here")
    );
    assert_eq!(
        db.apply(&node, "lantern", &removal(&db, &founder, &peer, 1), 10.0),
        Ok(Applied::Removed(peer))
    );
    assert_eq!(db.standing(&peer), Ok(Standing::Refused));
}
#[test]
fn applying_held_removals_are_bounded_and_errors_not_seen() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let signer = iroh::SecretKey::generate();
    computer(
        &db,
        &signer.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    for counter in 1..=64 {
        assert_eq!(
            db.apply(
                &node.public(),
                "lantern",
                &removal(&db, &signer, &iroh::SecretKey::generate().public(), counter),
                10.0
            ),
            Ok(Applied::Held)
        );
    }
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &signer, &iroh::SecretKey::generate().public(), 65),
            10.0
        ),
        Err("too many held removals from that computer")
    );
    assert_eq!(count(&db, "records"), 64);
    db.conn.execute("DELETE FROM held", []).unwrap();
    for n in 0..HELD_MAX {
        db.conn
            .execute(
                "INSERT INTO held VALUES (?1, ?2, 10.0)",
                params![format!("{n:064x}"), format!("{:064x}", n + 1)],
            )
            .unwrap();
    }
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &signer, &iroh::SecretKey::generate().public(), 65),
            10.0
        ),
        Err("too many held removals")
    );
    assert_eq!(count(&db, "records"), 64);
}
#[test]
fn applying_leave_of_known_computer_and_tombstone_is_final() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let peer = iroh::SecretKey::generate();
    computer(
        &db,
        &peer.public(),
        "orchard",
        State::Blocked,
        None,
        &addr(),
    );
    let record = removal(&db, &peer, &peer.public(), 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Ok(Applied::Removed(peer.public()))
    );
    assert_eq!(db.computers().unwrap()[0].state, State::Left);
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &peer, &peer.public(), 2),
            11.0
        ),
        Err("the computer was already removed")
    );
}
#[test]
fn applying_own_removal_clears_every_table_and_cannot_be_repeated() {
    let inviter = iroh::SecretKey::generate();
    let node = iroh::SecretKey::generate();
    let (code, answer) = answer(&inviter);
    let temp = Temp::new();
    let mut db = temp.open();
    db.joined(&node.public(), &code, &answer, 10.0).unwrap();
    mint(&mut db, &node, 10.0);
    let stranger = iroh::SecretKey::generate();
    computer(
        &db,
        &stranger.public(),
        "meadow",
        State::Accepted,
        None,
        &addr(),
    );
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &stranger, &node.public(), 1),
            10.0
        ),
        Err("only its inviter or the founding member removes a computer")
    );
    let record = removal(&db, &inviter, &node.public(), 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Ok(Applied::LeftFleet)
    );
    for table in [
        "fleet",
        "codes",
        "computers",
        "tombstones",
        "records",
        "held",
        "local",
    ] {
        assert_eq!(count(&db, table), 0);
    }
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 11.0),
        Err("not in a fleet")
    );
}

#[test]
fn local_accept_block_trust_and_target_rules() {
    let temp = Temp::new();
    let (mut db, _) = temp.found();
    let peer = iroh::SecretKey::generate().public();
    computer(&db, &peer, "orchard", State::Pending, None, &addr());
    assert_eq!(
        db.trust("orchard"),
        Err("only an accepted computer can be trusted")
    );
    let c = db.accept("orchard").unwrap();
    assert_eq!(c.state, State::Accepted);
    assert!(!c.trusted);
    assert_eq!(db.accept(&peer.to_string()).unwrap(), c);
    assert!(db.trust("orchard").unwrap().trusted);
    let c = db.block(&peer.to_string()).unwrap();
    assert_eq!(c.state, State::Blocked);
    assert!(!c.trusted);
    assert_eq!(
        db.trust("orchard"),
        Err("only an accepted computer can be trusted")
    );
    assert_eq!(db.accept("orchard").unwrap().state, State::Accepted);
    assert_eq!(count(&db, "records"), 0);
    for (target, phrase) in [
        ("garden".to_string(), "no such computer"),
        ("a".repeat(65), "not a computer"),
        ("AB".repeat(32), "not a computer"),
        ("Garden".into(), "not a computer"),
    ] {
        assert_eq!(db.block(&target), Err(phrase));
    }
    let unknown = iroh::SecretKey::generate().public();
    assert_eq!(db.block(&unknown.to_string()), Err("no such computer"));
    tombstone(&db, &unknown, &peer);
    assert_eq!(
        db.block(&unknown.to_string()),
        Err("the computer was removed")
    );
}
#[test]
fn local_changes_need_a_fleet() {
    let temp = Temp::new();
    let mut db = temp.open();
    let node = iroh::SecretKey::generate();
    assert_eq!(db.accept("orchard"), Err("not in a fleet"));
    assert_eq!(db.block("orchard"), Err("not in a fleet"));
    assert_eq!(db.trust("orchard"), Err("not in a fleet"));
    assert_eq!(db.remove(&node, "orchard", 10.0), Err("not in a fleet"));
    assert_eq!(db.leave(&node, 11.0), Err("not in a fleet"));
    assert_eq!(db.forget(), Err("not in a fleet"));
}
#[test]
fn local_remove_signs_next_counter_and_tombstones_invitee() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let peer_secret = iroh::SecretKey::generate();
    let peer = peer_secret.public();
    admit_new(&mut db, &node, &peer_secret, "orchard", 10.0);
    db.conn
        .execute(
            "INSERT INTO held VALUES (?1, ?2, 10.0)",
            params![node.public().to_string(), peer.to_string()],
        )
        .unwrap();
    let (record, key) = db.remove(&node, "orchard", 11.0).unwrap();
    assert_eq!(key, peer);
    assert_eq!(record.verify(), Ok(()));
    assert_eq!(record.counter(), 2);
    assert!(
        matches!(record.body, RecordBody::Removal {subject, ..} if subject == peer.to_string())
    );
    assert_eq!(count(&db, "held"), 0);
    assert_eq!(count(&db, "records"), 2);
    assert_eq!(db.standing(&peer), Ok(Standing::Refused));
    assert_eq!(db.computers().unwrap()[0].state, State::Removed);
    assert_eq!(
        db.remove(&node, &peer.to_string(), 12.0).err(),
        Some("the computer was removed")
    );
}
#[test]
fn local_remove_requires_inviter_or_founder() {
    let inviter = iroh::SecretKey::generate();
    let node = iroh::SecretKey::generate();
    let other = iroh::SecretKey::generate().public();
    let (code, answer) = answer(&inviter);
    let temp = Temp::new();
    let mut db = temp.open();
    db.joined(&node.public(), &code, &answer, 10.0).unwrap();
    computer(
        &db,
        &other,
        "meadow",
        State::Pending,
        Some(&inviter.public()),
        &addr(),
    );
    assert_eq!(
        db.remove(&node, "meadow", 10.0).err(),
        Some("only its inviter or the founding member removes a computer")
    );
    assert_eq!(count(&db, "records"), 0);
    db.conn
        .execute(
            "UPDATE computers SET invited_by = ?1 WHERE key = ?2",
            params![node.public().to_string(), other.to_string()],
        )
        .unwrap();
    assert!(db.remove(&node, "meadow", 10.0).is_ok());
    let temp = Temp::new();
    let (mut db, founder) = temp.found();
    computer(
        &db,
        &other,
        "meadow",
        State::Blocked,
        Some(&node.public()),
        &addr(),
    );
    assert!(db.remove(&founder, "meadow", 10.0).is_ok());
}
#[test]
fn local_leave_keeps_state_until_forgotten() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let peer_secret = iroh::SecretKey::generate();
    let peer = peer_secret.public();
    admit_new(&mut db, &node, &peer_secret, "orchard", 10.0);
    let pending = iroh::SecretKey::generate().public();
    computer(&db, &pending, "meadow", State::Pending, None, &addr());
    let blocked = iroh::SecretKey::generate().public();
    computer(&db, &blocked, "harbor", State::Blocked, None, &addr());
    let no_addr = iroh::SecretKey::generate().public();
    computer(&db, &no_addr, "juniper", State::Accepted, None, &[]);
    mint(&mut db, &node, 10.0);
    let fleet = db.fleet().unwrap();
    let computers = db.computers().unwrap();
    let codes = db.codes(10.0).unwrap();
    let (record, recipients) = db.leave(&node, 11.0).unwrap();
    assert_eq!(record.verify(), Ok(()));
    assert_eq!(record.counter(), 2);
    assert!(
        matches!(record.body, RecordBody::Removal {signer, subject, ..} if signer == subject && signer == node.public().to_string())
    );
    assert_eq!(recipients, vec![(peer, addr())]);
    assert_eq!(db.fleet().unwrap(), fleet);
    assert_eq!(db.computers().unwrap(), computers);
    assert_eq!(db.codes(10.0).unwrap(), codes);
    assert_eq!(count(&db, "records"), 2);
    let kind: String = db
        .conn
        .query_row(
            "SELECT kind FROM records WHERE signer = ?1 AND counter = ?2",
            params![node.public().to_string(), 2i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "removal");
    let at: f64 = db
        .conn
        .query_row("SELECT at FROM records WHERE counter = 2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(at, 11.0);
    db.forget().unwrap();
    assert_eq!(db.fleet().unwrap(), None);
    assert!(db.computers().unwrap().is_empty());
    assert!(db.codes(10.0).unwrap().is_empty());
    for table in [
        "fleet",
        "codes",
        "computers",
        "tombstones",
        "records",
        "held",
        "local",
    ] {
        assert_eq!(count(&db, table), 0);
    }
}

#[test]
fn admitting_oversize_answer_rolls_back_code_computer_counter_and_record() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let newcomer_secret = iroh::SecretKey::generate();
    let newcomer = newcomer_secret.public();
    for n in 0..=REMOVED_MAX {
        db.conn
            .execute(
                "INSERT INTO tombstones VALUES (?1, NULL, ?2, 10.0)",
                params![format!("{n:064x}"), node.public().to_string()],
            )
            .unwrap();
    }
    let (code, _) = mint(&mut db, &node, 10.0);
    assert_eq!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &newcomer,
            "orchard",
            &addr(),
            &membership::consent(
                &newcomer_secret,
                &code.fleet,
                &node.public().to_string(),
                "orchard",
                &addr()
            ),
            10.0
        )
        .err(),
        Some("the fleet is too large to join")
    );
    assert!(db.join_open(10.0).unwrap());
    assert_eq!(count(&db, "computers"), 0);
    assert_eq!(count(&db, "records"), 0);
    assert_eq!(
        db.conn
            .query_row("SELECT next_counter FROM local", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.conn.execute("DELETE FROM tombstones", []).unwrap();
    assert_eq!(
        db.admit(
            &node,
            "lantern",
            &addr(),
            &code,
            &newcomer,
            "orchard",
            &addr(),
            &membership::consent(
                &newcomer_secret,
                &code.fleet,
                &node.public().to_string(),
                "orchard",
                &addr()
            ),
            10.0
        )
        .unwrap()
        .1
        .unwrap()
        .counter(),
        1
    );
}

#[test]
fn applying_and_local_removal_roll_back_when_record_cannot_be_written() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    let newcomer_secret = iroh::SecretKey::generate();
    let newcomer = newcomer_secret.public();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    db.conn.execute_batch("CREATE TRIGGER refuse_record BEFORE INSERT ON records BEGIN SELECT RAISE(ABORT, 'refuse'); END;").unwrap();
    let introduction = intro(&db, &inviter, &newcomer_secret, "garden", 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &introduction, 10.0),
        Err(WRITE)
    );
    assert_eq!(count(&db, "computers"), 1);
    assert_eq!(count(&db, "records"), 0);
    computer(
        &db,
        &newcomer,
        "garden",
        State::Accepted,
        Some(&inviter.public()),
        &addr(),
    );
    let record = removal(&db, &inviter, &newcomer, 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Err(WRITE)
    );
    assert_eq!(db.standing(&newcomer), Ok(Standing::Accepted));
    assert_eq!(count(&db, "tombstones"), 0);
    assert_eq!(db.remove(&node, "garden", 10.0).err(), Some(WRITE));
    assert_eq!(db.standing(&newcomer), Ok(Standing::Accepted));
    assert_eq!(count(&db, "tombstones"), 0);
    assert_eq!(
        db.conn
            .query_row("SELECT next_counter FROM local", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn a_founder_that_left_cannot_remove_any_computer() {
    for subject_kind in 0..3 {
        let inviter = iroh::SecretKey::generate();
        let node = iroh::SecretKey::generate();
        let (code, answer) = answer(&inviter);
        let temp = Temp::new();
        let mut db = temp.open();
        db.joined(&node.public(), &code, &answer, 10.0).unwrap();
        let peer = iroh::SecretKey::generate().public();
        computer(
            &db,
            &peer,
            "meadow",
            State::Accepted,
            Some(&inviter.public()),
            &addr(),
        );
        let leave = removal(&db, &inviter, &inviter.public(), 1);
        assert_eq!(
            db.apply(&node.public(), "lantern", &leave, 10.0),
            Ok(Applied::Removed(inviter.public()))
        );
        let before = db.computers().unwrap();
        let records = count(&db, "records");
        let subject = match subject_kind {
            0 => peer,
            1 => node.public(),
            _ => iroh::SecretKey::generate().public(),
        };
        let record = removal(&db, &inviter, &subject, 2);
        assert_eq!(
            db.apply(&node.public(), "lantern", &record, 11.0),
            Err("the signer is not accepted here")
        );
        assert_eq!(db.computers().unwrap(), before);
        assert_eq!(count(&db, "records"), records);
        assert!(db.fleet().unwrap().is_some());
    }
}

#[test]
fn a_founder_tombstone_clears_held_removals() {
    let temp = Temp::new();
    let (mut db, founder) = temp.found();
    let node = iroh::SecretKey::generate().public();
    let signer = iroh::SecretKey::generate();
    let subject = iroh::SecretKey::generate().public();
    computer(
        &db,
        &signer.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    assert_eq!(
        db.apply(&node, "lantern", &removal(&db, &signer, &subject, 1), 10.0),
        Ok(Applied::Held)
    );
    assert_eq!(count(&db, "held"), 1);
    assert_eq!(
        db.apply(&node, "lantern", &removal(&db, &founder, &subject, 1), 11.0),
        Ok(Applied::Removed(subject))
    );
    assert_eq!(count(&db, "held"), 0);
}

#[test]
fn an_introduction_cannot_take_this_computers_name() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    let peer_secret = iroh::SecretKey::generate();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    db.trust("orchard").unwrap();
    let before = db.computers().unwrap();
    let record = intro(&db, &inviter, &peer_secret, "lantern", 1);
    assert_eq!(
        db.apply(&node.public(), "lantern", &record, 10.0),
        Err("the name is taken here")
    );
    assert_eq!(db.computers().unwrap(), before);
    assert_eq!(count(&db, "records"), 0);
    let (answer, _) = admit_new(&mut db, &node, &peer_secret, "meadow", 11.0);
    let names: HashSet<_> = answer.computers.iter().map(|c| &c.name).collect();
    assert_eq!(names.len(), answer.computers.len());
}

#[test]
fn admitting_requires_the_newcomers_signed_request_and_keeps_the_code_open() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let newcomer = iroh::SecretKey::generate();
    let other = iroh::SecretKey::generate();
    let (code, _) = mint(&mut db, &node, 10.0);
    for bad in [
        membership::consent(
            &other,
            &code.fleet,
            &node.public().to_string(),
            "orchard",
            &addr(),
        ),
        membership::consent(
            &newcomer,
            &code.fleet,
            &node.public().to_string(),
            "meadow",
            &addr(),
        ),
    ] {
        assert_eq!(
            db.admit(
                &node,
                "lantern",
                &addr(),
                &code,
                &newcomer.public(),
                "orchard",
                &addr(),
                &bad,
                10.0
            ),
            Err("the join request is not signed")
        );
        assert!(db.join_open(10.0).unwrap());
        assert_eq!(count(&db, "computers"), 0);
        assert_eq!(count(&db, "records"), 0);
    }
}
#[test]
fn a_hostile_computer_cannot_claim_another_inviters_newcomer() {
    let temp = Temp::new();
    let (mut db, node) = temp.found();
    let inviter = iroh::SecretKey::generate();
    let hostile = iroh::SecretKey::generate();
    let newcomer = iroh::SecretKey::generate();
    computer(
        &db,
        &inviter.public(),
        "orchard",
        State::Accepted,
        None,
        &addr(),
    );
    computer(
        &db,
        &hostile.public(),
        "meadow",
        State::Accepted,
        None,
        &addr(),
    );
    let real = intro(&db, &inviter, &newcomer, "harbor", 1);
    let mut body = real.body.clone();
    if let RecordBody::Introduction { inviter, .. } = &mut body {
        *inviter = hostile.public().to_string();
    }
    let forged = membership::sign(body, &hostile);
    assert_eq!(
        db.apply(&node.public(), "lantern", &forged, 10.0),
        Err("not a membership record")
    );
    assert_eq!(count(&db, "records"), 0);
    assert_eq!(
        db.apply(&node.public(), "lantern", &real, 10.0),
        Ok(Applied::Added { pending: true })
    );
    assert_eq!(
        db.computer("harbor").unwrap().invited_by,
        Some(inviter.public().to_string())
    );
    assert_eq!(
        db.apply(
            &node.public(),
            "lantern",
            &removal(&db, &hostile, &newcomer.public(), 2),
            11.0
        ),
        Err(REMOVE_AUTH)
    );
}

#[test]
fn joining_refuses_more_than_the_computer_limit() {
    let inviter = iroh::SecretKey::generate();
    let node = iroh::SecretKey::generate();
    let (code, mut answer) = answer(&inviter);
    answer.computers.truncate(1);
    for n in 0..COMPUTERS_MAX {
        answer.computers.push(Known {
            key: iroh::SecretKey::generate().public().to_string(),
            name: format!("computer-{n}"),
            invited_by: Some(inviter.public().to_string()),
            addrs: addr(),
        });
    }
    assert_eq!(answer.computers.len(), COMPUTERS_MAX + 1);
    let temp = Temp::new();
    let mut db = temp.open();
    assert_eq!(
        db.joined(&node.public(), &code, &answer, 10.0),
        Err("the join answer is not valid")
    );
    for table in ["fleet", "computers", "local", "tombstones"] {
        assert_eq!(count(&db, table), 0);
    }
}
