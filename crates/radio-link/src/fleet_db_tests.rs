use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
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
fn opens_private_version_one_and_reopens_data() {
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
    for version in [2, -1] {
        let temp = Temp::new();
        let db = temp.open();
        db.conn
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(db);
        assert_eq!(
            FleetDb::open(&temp.path()).err(),
            Some(if version == 2 {
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
