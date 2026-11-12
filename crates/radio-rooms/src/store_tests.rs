use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Store(PathBuf);

impl Store {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "radio-rooms-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> PathBuf {
        self.0.join("rooms/rooms.db")
    }

    fn open(&self) -> RoomStore {
        RoomStore::open(&self.path()).unwrap()
    }

    fn room(&self) -> RoomStore {
        let mut store = self.open();
        store.create_room("room", "node-a", "Room", 1.0).unwrap();
        store
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn author() -> Author {
    Author::member("helper", "juniper")
}

#[test]
fn fresh_store_is_private_and_reopens_without_repeating_migrations() {
    let fixture = Store::new("fresh");
    let store = fixture.open();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(fixture.path().parent().unwrap()), 0o700);
    assert_eq!(mode(&fixture.path()), 0o600);
    let journal: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    let fk: u32 = store
        .conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    drop(store);
    std::fs::set_permissions(
        fixture.path().parent().unwrap(),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let store = fixture.open();
    assert_eq!(mode(fixture.path().parent().unwrap()), 0o700);
    let versions: Vec<u32> = store
        .conn
        .prepare("SELECT version FROM schema_version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(versions, [3]);
}

#[test]
fn opening_an_existing_store_tightens_file_permissions() {
    let fixture = Store::new("existing-mode");
    fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    fs::write(fixture.path(), "").unwrap();
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o644)).unwrap();
    let _store = fixture.open();
    assert_eq!(
        fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn newer_store_is_refused() {
    let fixture = Store::new("future");
    let store = fixture.open();
    store
        .conn
        .execute("UPDATE schema_version SET version = 99", [])
        .unwrap();
    drop(store);
    assert_eq!(
        RoomStore::open(&fixture.path()).err().unwrap(),
        "the room store is newer than this build"
    );
}

#[test]
fn a_linked_database_or_parent_is_refused() {
    let fixture = Store::new("links");
    std::fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    let target = fixture.0.join("target");
    std::fs::write(&target, "").unwrap();
    symlink(&target, fixture.path()).unwrap();
    assert!(RoomStore::open(&fixture.path()).is_err());
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
    std::fs::remove_file(fixture.path()).unwrap();
    std::fs::remove_dir(fixture.path().parent().unwrap()).unwrap();
    symlink(&fixture.0, fixture.path().parent().unwrap()).unwrap();
    assert!(RoomStore::open(&fixture.path()).is_err());
    assert!(!fixture.0.join("rooms.db").exists());
}

#[test]
fn rooms_check_ids_and_clean_titles() {
    let fixture = Store::new("rooms");
    let mut store = fixture.open();
    for id in ["", "a/b", "two words", "é", &"a".repeat(65)] {
        assert!(store.create_room(id, "node-a", "Room", 1.0).is_err());
    }
    assert!(store.create_room("empty", "node-a", "\0\t ", 1.0).is_err());
    store
        .create_room("A.z_9-", "node-a", &format!("\0{}", "t".repeat(121)), 1.0)
        .unwrap();
    let title: String = store
        .conn
        .query_row("SELECT title FROM rooms WHERE id = ?1", ["A.z_9-"], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, format!("{}…", "t".repeat(120)));
    assert!(
        store
            .create_room("A.z_9-", "node-a", "Duplicate", 2.0)
            .is_err()
    );
}

#[test]
fn posts_sequence_per_room_and_preserve_authors_and_times() {
    let fixture = Store::new("posts");
    let mut store = fixture.room();
    store.create_room("other", "node-b", "Other", 1.0).unwrap();
    for seq in 1..=3 {
        assert_eq!(
            store.post("room", "node-a", &author(), "hello", seq as f64),
            Ok(seq)
        );
    }
    let human = Author::human("sam");
    assert_eq!(store.post("other", "node-b", &human, "hi", 4.0), Ok(1));
    assert!(
        store
            .post("missing", "node-a", &author(), "hello", 5.0)
            .is_err()
    );
    assert_eq!(
        store.tail("other", 1).unwrap(),
        [Message {
            seq: 1,
            kind: MessageKind::Post,
            author_host: "node-b".into(),
            author: human,
            body: "hi".into(),
            sent_at: 4.0,
            delivered_at: None,
        }]
    );
}

#[test]
fn bodies_are_cleaned_and_limited_in_bytes_without_cutting() {
    let fixture = Store::new("bodies");
    let mut store = fixture.room();
    assert_eq!(BODY_MAX_BYTES, 4096);
    assert!(
        store
            .post("room", "node-a", &author(), &"é".repeat(2048), 1.0)
            .is_ok()
    );
    assert!(
        store
            .post("room", "node-a", &author(), &"a".repeat(4097), 1.0)
            .is_err()
    );
    assert!(
        store
            .post("room", "node-a", &author(), " \0\u{7}\n\t ", 1.0)
            .is_err()
    );
    assert_eq!(
        store.post("room", "node-a", &author(), " a\0b\u{7}c\n\td ", 2.0),
        Ok(2)
    );
    let rows = store.tail("room", 5).unwrap();
    assert_eq!(rows[0].body.len(), 4096);
    assert_eq!(rows[1].body, "a b c\n\td");
}

#[test]
fn status_checks_keys_and_counts_unicode_characters() {
    let fixture = Store::new("status-lines");
    let mut store = fixture.room();
    assert_eq!(STATUS_LINE_MAX_CHARS, 200);
    assert_eq!(KEY_MAX_CHARS, 40);
    for key in ["", "a/b", "two words", "é", &"k".repeat(41)] {
        assert!(
            store
                .set_status(
                    "room",
                    key,
                    "ok",
                    "node-a",
                    &Author::member("helper", "juniper"),
                    None,
                    1.0
                )
                .is_err()
        );
    }
    assert!(
        store
            .set_status(
                "missing",
                "key",
                "ok",
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                1.0
            )
            .is_err()
    );
    assert!(
        store
            .set_status(
                "room",
                "key",
                " \0\n\t ",
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                1.0
            )
            .is_err()
    );
    store
        .set_status(
            "room",
            "A.z_9-",
            &"é".repeat(200),
            "node-a",
            &Author::member("helper", "juniper"),
            None,
            1.0,
        )
        .unwrap();
    assert!(
        store
            .set_status(
                "room",
                "A.z_9-",
                &"x".repeat(201),
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                2.0
            )
            .is_err()
    );
    store
        .set_status(
            "room",
            &"k".repeat(40),
            " a\nb\tc\0d ",
            "node-a",
            &Author::member("helper", "juniper"),
            Some(5.0),
            2.0,
        )
        .unwrap();
    let rows = store.status("room", 2.0).unwrap();
    assert_eq!(rows[0].line, "é".repeat(200));
    assert_eq!(
        rows[1],
        StatusLine {
            key: "k".repeat(40),
            line: "a b c d".into(),
            set_by: Author::member("helper", "juniper"),
            set_at: 2.0,
            expires_at: Some(5.0)
        }
    );
}

#[test]
fn status_key_cap_replacements_expiry_and_clear() {
    let fixture = Store::new("status-keys");
    let mut store = fixture.room();
    assert_eq!(STATUS_MAX_KEYS, 20);
    for i in 0..20 {
        store
            .set_status(
                "room",
                &format!("k{i:02}"),
                "ok",
                "node-a",
                &Author::member("helper", "juniper"),
                Some(5.0),
                1.0,
            )
            .unwrap();
    }
    assert!(
        store
            .set_status(
                "room",
                "extra",
                "ok",
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                1.0
            )
            .is_err()
    );
    store
        .set_status(
            "room",
            "k00",
            "replaced",
            "node-a",
            &Author::human("sam"),
            None,
            2.0,
        )
        .unwrap();
    assert_eq!(store.status("room", 2.0).unwrap().len(), 20);
    assert_eq!(store.status("room", 5.0).unwrap().len(), 1);
    store
        .set_status(
            "room",
            "extra",
            "ok",
            "node-a",
            &Author::member("helper", "juniper"),
            None,
            5.0,
        )
        .unwrap();
    let rows = store.status("room", 5.0).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        ["extra", "k00"]
    );
    assert_eq!(
        store.clear_status("room", "extra", "node-a", &author(), 5.0),
        Ok(Some(23))
    );
    assert_eq!(
        store.clear_status("room", "extra", "node-a", &author(), 5.0),
        Ok(None)
    );
}

#[test]
fn status_byte_cap_counts_replacements_once_and_drops_expired_lines() {
    let fixture = Store::new("status-bytes");
    let mut store = fixture.room();
    assert_eq!(STATUS_MAX_BYTES, 2048);
    for i in 0..5 {
        store
            .set_status(
                "room",
                &format!("k{i}"),
                &"é".repeat(200),
                "node-a",
                &Author::member("helper", "juniper"),
                Some(5.0),
                1.0,
            )
            .unwrap();
    }
    store
        .set_status(
            "room",
            "edge",
            &"x".repeat(48),
            "node-a",
            &Author::member("helper", "juniper"),
            None,
            1.0,
        )
        .unwrap();
    store
        .set_status(
            "room",
            "k0",
            &"é".repeat(200),
            "node-a",
            &Author::member("helper", "juniper"),
            Some(5.0),
            2.0,
        )
        .unwrap();
    assert!(
        store
            .set_status(
                "room",
                "edge",
                &"x".repeat(49),
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                2.0
            )
            .is_err()
    );
    assert!(
        store
            .set_status(
                "room",
                "extra",
                "x",
                "node-a",
                &Author::member("helper", "juniper"),
                None,
                2.0
            )
            .is_err()
    );
    assert_eq!(store.status("room", 2.0).unwrap()[0].line.len(), 48);
    store
        .set_status(
            "room",
            "extra",
            &"é".repeat(200),
            "node-a",
            &Author::member("helper", "juniper"),
            None,
            5.0,
        )
        .unwrap();
    assert_eq!(store.status("room", 5.0).unwrap().len(), 2);
}

#[test]
fn tail_is_newest_n_oldest_first_and_capped_at_500() {
    let fixture = Store::new("tail");
    let mut store = fixture.room();
    for i in 1..=503 {
        store
            .post("room", "node-a", &author(), "hi", i as f64)
            .unwrap();
    }
    let seqs = |limit| {
        store
            .tail("room", limit)
            .unwrap()
            .iter()
            .map(|m| m.seq)
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(3), [501, 502, 503]);
    assert!(seqs(0).is_empty());
    assert_eq!(seqs(usize::MAX), (4..=503).collect::<Vec<_>>());
    assert!(store.tail("missing", 10).unwrap().is_empty());
}

#[test]
#[ignore]
fn throughput() {
    let fixture = Store::new("throughput");
    let mut store = fixture.room();
    let body = "x".repeat(200);
    let started = Instant::now();
    for i in 0..10_000 {
        store
            .post("room", "node-a", &author(), &body, i as f64)
            .unwrap();
    }
    let posts = 10_000.0 / started.elapsed().as_secs_f64();
    let started = Instant::now();
    for _ in 0..1_000 {
        assert_eq!(store.tail("room", 50).unwrap().len(), 50);
    }
    let reads = 1_000.0 / started.elapsed().as_secs_f64();
    eprintln!("rooms throughput: {posts:.0} posts/s, {reads:.0} tail reads/s");
    assert!(posts >= 100.0, "{posts:.0} posts/s is below 100");
    assert!(reads >= 100.0, "{reads:.0} reads/s is below 100");
}

#[test]
fn names_are_bounded_for_every_field() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for text in ["", " \t\n ", &"é".repeat(65), &"é".repeat(64)] {
        let fixture = Store::new("names");
        let mut store = fixture.room();
        let results = [
            store.create_room("other", text, "Other", 1.0),
            store.post("room", text, &author(), "hi", 1.0).map(|_| ()),
            store
                .post("room", "node-a", &Author::member("helper", text), "hi", 1.0)
                .map(|_| ()),
            store
                .post("room", "node-a", &Author::human(text), "hi", 1.0)
                .map(|_| ()),
            store
                .set_status(
                    "room",
                    "key",
                    "ok",
                    "node-a",
                    &Author::member("helper", text),
                    None,
                    1.0,
                )
                .map(|_| ()),
        ];
        actual.extend(results);
        let result = if text.chars().count() == 64 {
            Ok(())
        } else {
            Err("not a name".into())
        };
        expected.extend(vec![result; 5]);
    }
    assert_eq!(actual, expected);
}

#[test]
fn names_are_cleaned_before_storage() {
    let fixture = Store::new("clean-names");
    let mut store = fixture.open();
    store
        .create_room("room", " \0node-a\0 ", "Room", 1.0)
        .unwrap();
    store
        .post(
            "room",
            " \0node-a\0 ",
            &Author::member("helper", " \0juniper\0 "),
            "hi",
            1.0,
        )
        .unwrap();
    store
        .post(
            "room",
            " \0node-a\0 ",
            &Author::human(" \0sam\0 "),
            "hi",
            1.0,
        )
        .unwrap();
    store
        .set_status(
            "room",
            "key",
            "ok",
            "node-a",
            &Author::member("helper", " \0juniper\0 "),
            None,
            1.0,
        )
        .unwrap();
    let home: String = store
        .conn
        .query_row("SELECT home FROM rooms WHERE id = 'room'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let messages = store.since("room", 0, 2).unwrap();
    let status = store.status("room", 1.0).unwrap();
    assert_eq!(
        (
            home.as_str(),
            messages[0].author_host.as_str(),
            &messages[0].author,
            &messages[1].author,
            &status[0].set_by
        ),
        (
            "node-a",
            "node-a",
            &Author::member("helper", "juniper"),
            &Author::human("sam"),
            &Author::member("helper", "juniper")
        )
    );
}

#[test]
fn names_replace_inner_newlines_and_tabs_before_storage() {
    for text in ["mi\nmi", "mi\tmi"] {
        let fixture = Store::new("single-line-names");
        let mut store = fixture.open();
        store.create_room("room", text, "Room", 1.0).unwrap();
        for author in [Author::member("helper", text), Author::human(text)] {
            store.post("room", text, &author, "hi", 1.0).unwrap();
        }
        store
            .set_status(
                "room",
                "key",
                "ok",
                "node-a",
                &Author::member("helper", text),
                None,
                1.0,
            )
            .unwrap();
        let home: String = store
            .conn
            .query_row("SELECT home FROM rooms WHERE id = 'room'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let messages = store.since("room", 0, 2).unwrap();
        let status = store.status("room", 1.0).unwrap();
        assert_eq!(home, "mi mi");
        assert_eq!(messages[0].author_host, "mi mi");
        assert_eq!(messages[1].author_host, "mi mi");
        assert_eq!(messages[0].author, Author::member("helper", "mi mi"));
        assert_eq!(messages[1].author, Author::human("mi mi"));
        assert_eq!(status[0].set_by, Author::member("helper", "mi mi"));
    }
}

#[test]
fn times_must_be_finite_at_every_boundary() {
    let mut actual = Vec::new();
    for at in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let fixture = Store::new("times");
        let mut store = fixture.room();
        actual.extend([
            store.create_room("other", "node-a", "Other", at),
            store
                .post("room", "node-a", &author(), "hi", at)
                .map(|_| ()),
            store
                .set_status("room", "key", "ok", "node-a", &author(), None, at)
                .map(|_| ()),
            store
                .set_status("room", "expiry", "ok", "node-a", &author(), Some(at), 1.0)
                .map(|_| ()),
            store.status("room", at).map(|_| ()),
        ]);
    }
    assert_eq!(actual, vec![Err("not a time".into()); 15]);
}

fn human() -> Author {
    Author::human("sam")
}

#[test]
fn a_version_1_store_migrates_to_3_and_keeps_its_rows() {
    let fixture = Store::new("migrate-1");
    std::fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    let conn = Connection::open(fixture.path()).unwrap();
    conn.execute_batch(&migration_sql(0)).unwrap();
    conn.execute_batch(
        "INSERT INTO schema_version (version) VALUES (1);
         INSERT INTO rooms VALUES ('room', 'node-a', 'Room', 1.0);
         INSERT INTO messages (room, seq, author_host, author_kind, author, body, sent_at)
             VALUES ('room', 1, 'node-a', 'helper', 'juniper', 'hi', 1.0);
         INSERT INTO room_status (room, key, line, set_by, set_at) VALUES ('room', 'work', 'Ready', 'sam', 1.0);",
    )
    .unwrap();
    drop(conn);
    let mut store = fixture.open();
    assert_eq!(store.tail("room", 5).unwrap()[0].kind, MessageKind::Post);
    assert_eq!(store.status("room", 1.0).unwrap()[0].set_by, human());
    assert_eq!(
        store.set_status("room", "work", "Mine", "node-a", &author(), None, 2.0),
        Err(OWNED.into())
    );
}

#[test]
fn setting_and_clearing_status_are_numbered_events_in_the_chat_log() {
    let fixture = Store::new("events");
    let mut store = fixture.room();
    assert_eq!(store.post("room", "node-a", &human(), "hi", 1.0), Ok(1));
    assert_eq!(
        store.set_status("room", "work", "Ready", "node-b", &author(), Some(9.0), 2.0),
        Ok(2)
    );
    assert_eq!(
        store.clear_status("room", "work", "node-b", &author(), 3.0),
        Ok(Some(3))
    );
    assert_eq!(
        store.clear_status("room", "work", "node-b", &author(), 3.0),
        Ok(None)
    );
    let events = store.since("room", 1, 10).unwrap();
    assert_eq!(
        events,
        [
            Message {
                seq: 2,
                kind: MessageKind::StatusSet,
                author_host: "node-b".into(),
                author: author(),
                body: "work: Ready".into(),
                sent_at: 2.0,
                delivered_at: None,
            },
            Message {
                seq: 3,
                kind: MessageKind::StatusClear,
                author_host: "node-b".into(),
                author: author(),
                body: "work".into(),
                sent_at: 3.0,
                delivered_at: None,
            },
        ]
    );
    assert!(
        store
            .set_status("room", "work", "x", "node-b", &author(), Some(3.0), 3.0)
            .is_err()
    );
    assert_eq!(store.room("room").unwrap().unwrap().last_seq, 3);
}

#[test]
fn a_helper_changes_only_her_own_lines_and_the_human_changes_any() {
    let fixture = Store::new("owners");
    let mut store = fixture.room();
    let sam = Author::member("helper", "sam");
    store
        .set_status("room", "juniper", "N", "node-a", &author(), None, 1.0)
        .unwrap();
    store
        .set_status("room", "human", "H", "node-a", &human(), None, 1.0)
        .unwrap();
    for key in ["juniper", "human"] {
        assert_eq!(
            store.set_status("room", key, "taken", "node-a", &sam, None, 2.0),
            Err(OWNED.into())
        );
        assert_eq!(
            store.clear_status("room", key, "node-a", &sam, 2.0),
            Err(OWNED.into())
        );
    }
    assert_eq!(
        store.set_status(
            "room",
            "human",
            "taken",
            "node-a",
            &Author::member("helper", "sam"),
            None,
            2.0
        ),
        Err(OWNED.into())
    );
    store
        .set_status("room", "juniper", "N2", "node-a", &author(), None, 2.0)
        .unwrap();
    assert!(
        store
            .clear_status("room", "juniper", "node-a", &author(), 2.0)
            .unwrap()
            .is_some()
    );
    store
        .set_status("room", "juniper", "N3", "node-a", &author(), None, 2.0)
        .unwrap();
    store
        .set_status(
            "room",
            "juniper",
            "by sam the human",
            "node-a",
            &human(),
            None,
            3.0,
        )
        .unwrap();
    assert!(
        store
            .clear_status("room", "human", "node-a", &human(), 3.0)
            .unwrap()
            .is_some()
    );
    let rows = store.status("room", 3.0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].line.as_str(), &rows[0].set_by),
        ("by sam the human", &human())
    );
    assert_eq!(
        store.set_status("room", "juniper", "back", "node-a", &author(), None, 4.0),
        Err(OWNED.into())
    );
}

#[test]
fn an_expired_line_is_gone_for_its_owner_check_and_its_clear() {
    let fixture = Store::new("expired-owner");
    let mut store = fixture.room();
    store
        .set_status("room", "work", "N", "node-a", &author(), Some(5.0), 1.0)
        .unwrap();
    store
        .set_status("room", "gone", "N", "node-a", &author(), Some(5.0), 1.0)
        .unwrap();
    let sam = Author::member("helper", "sam");
    assert_eq!(
        store.clear_status("room", "gone", "node-a", &sam, 5.0),
        Ok(None)
    );
    assert!(
        store
            .set_status("room", "work", "S", "node-a", &sam, None, 5.0)
            .is_ok()
    );
    assert_eq!(store.status("room", 5.0).unwrap()[0].set_by, sam);
}

#[test]
fn an_expiry_must_be_after_now() {
    let fixture = Store::new("expiry-now");
    let mut store = fixture.room();
    for at in [1.0, 0.5] {
        assert_eq!(
            store.set_status("room", "work", "N", "node-a", &author(), Some(at), 1.0),
            Err("the expiry is not in the future".into())
        );
    }
    assert!(store.status("room", 0.0).unwrap().is_empty());
}

#[test]
fn clear_checks_its_key_room_names_and_time() {
    let fixture = Store::new("clear-checks");
    let mut store = fixture.room();
    assert_eq!(
        store.clear_status("room", "a/b", "node-a", &author(), 1.0),
        Err("not a status key".into())
    );
    assert_eq!(
        store.clear_status("missing", "key", "node-a", &author(), 1.0),
        Err("no such room".into())
    );
    assert_eq!(
        store.clear_status("room", "key", "", &author(), 1.0),
        Err("not a name".into())
    );
    assert_eq!(
        store.clear_status("room", "key", "node-a", &author(), f64::NAN),
        Err("not a time".into())
    );
}

#[test]
fn ensure_room_creates_once_and_rooms_list_with_their_last_number() {
    let fixture = Store::new("ensure");
    let mut store = fixture.open();
    assert_eq!(
        store.ensure_room("z-node-a", "node-a", "Room room on node-a", 1.0),
        Ok(true)
    );
    assert_eq!(
        store.ensure_room("z-node-a", "node-a", "Another title", 2.0),
        Ok(false)
    );
    assert!(store.ensure_room("a/b", "node-a", "Bad", 1.0).is_err());
    store.create_room("room", "node-b", "Room", 3.0).unwrap();
    store.post("room", "node-b", &author(), "hi", 4.0).unwrap();
    store.post("room", "node-b", &author(), "hi", 5.0).unwrap();
    assert_eq!(
        store.rooms().unwrap(),
        [
            RoomRow {
                id: "room".into(),
                home: "node-b".into(),
                title: "Room".into(),
                created: 3.0,
                last_seq: 2,
            },
            RoomRow {
                id: "z-node-a".into(),
                home: "node-a".into(),
                title: "Room room on node-a".into(),
                created: 1.0,
                last_seq: 0,
            },
        ]
    );
    assert_eq!(
        store.room("z-node-a").unwrap().unwrap().title,
        "Room room on node-a"
    );
    assert_eq!(store.room("missing").unwrap(), None);
}

#[test]
fn since_is_after_n_oldest_first_and_capped_at_500() {
    let fixture = Store::new("since");
    let mut store = fixture.room();
    for i in 1..=503 {
        store
            .post("room", "node-a", &author(), "hi", i as f64)
            .unwrap();
    }
    let seqs = |after, limit| {
        store
            .since("room", after, limit)
            .unwrap()
            .iter()
            .map(|m| m.seq)
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(500, 10), [501, 502, 503]);
    assert_eq!(seqs(0, 2), [1, 2]);
    assert_eq!(seqs(0, usize::MAX), (1..=500).collect::<Vec<_>>());
    assert!(seqs(503, 10).is_empty());
    assert!(seqs(u64::MAX, 10).is_empty());
    assert!(store.since("missing", 0, 10).unwrap().is_empty());
}

fn earlier(fixture: &Store, version: usize) {
    fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    let conn = Connection::open(fixture.path()).unwrap();
    conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);
        CREATE TABLE rooms (id TEXT PRIMARY KEY, home TEXT NOT NULL, title TEXT NOT NULL, created REAL NOT NULL) STRICT;
        CREATE TABLE messages (
            room TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL, author_host TEXT NOT NULL,
            author_kind TEXT NOT NULL CHECK (author_kind IN ('helper','human')),
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
            author_kind TEXT NOT NULL CHECK (author_kind IN ('helper','human')),
            author TEXT NOT NULL, body TEXT NOT NULL, queued_at REAL NOT NULL
        ) STRICT;
        INSERT INTO rooms VALUES ('room', 'node-a', 'Room', 1.0);
        INSERT INTO messages VALUES ('room', 1, 'node-a', 'helper', 'juniper', 'hi', 2.0, 3.0);
        INSERT INTO room_status VALUES ('room', 'ready', 'Ready', 'sam', 4.0, 20.0);
        INSERT INTO outbox VALUES (7, 'room', 'node-b', 'helper', 'juniper', 'queued', 5.0);").unwrap();
    if version == 2 {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'post' CHECK (kind IN ('post','status_set','status_clear'));
            ALTER TABLE room_status ADD COLUMN set_by_kind TEXT NOT NULL DEFAULT 'human' CHECK (set_by_kind IN ('helper','human'));
            INSERT INTO messages VALUES ('room', 2, 'node-a', 'human', 'sam', 'ready: Ready', 4.0, NULL, 'status_set');
            INSERT INTO messages VALUES ('room', 3, 'node-a', 'human', 'sam', 'ready', 5.0, NULL, 'status_clear');").unwrap();
    }
    conn.execute("INSERT INTO schema_version VALUES (?1)", [version as i64])
        .unwrap();
}

fn schema(store: &RoomStore) -> Vec<(String, String)> {
    store
        .conn()
        .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| {
            let sql: String = row.get(1)?;
            Ok((row.get(0)?, sql.split_whitespace().collect::<String>()))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn check_earlier(version: usize) {
    let fixture = Store::new("earlier");
    earlier(&fixture, version);
    let mut store = fixture.open();
    assert_eq!(
        store
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        store.rooms().unwrap(),
        [RoomRow {
            id: "room".into(),
            home: "node-a".into(),
            title: "Room".into(),
            created: 1.0,
            last_seq: if version == 1 { 1 } else { 3 }
        }]
    );
    let mut expected = vec![Message {
        seq: 1,
        kind: MessageKind::Post,
        author_host: "node-a".into(),
        author: author(),
        body: "hi".into(),
        sent_at: 2.0,
        delivered_at: Some(3.0),
    }];
    if version == 2 {
        for (seq, kind, body, sent_at) in [
            (2, MessageKind::StatusSet, "ready: Ready", 4.0),
            (3, MessageKind::StatusClear, "ready", 5.0),
        ] {
            expected.push(Message {
                seq,
                kind,
                author_host: "node-a".into(),
                author: human(),
                body: body.into(),
                sent_at,
                delivered_at: None,
            });
        }
    }
    assert_eq!(store.tail("room", 20).unwrap(), expected);
    assert_eq!(
        store.status("room", 6.0).unwrap(),
        [StatusLine {
            key: "ready".into(),
            line: "Ready".into(),
            set_by: human(),
            set_at: 4.0,
            expires_at: Some(20.0)
        }]
    );
    let queued = store
        .conn()
        .query_row(
            "SELECT id, room, home, author_kind, author, body, queued_at FROM outbox",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, f64>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        queued,
        (
            7,
            "room".into(),
            "node-b".into(),
            "helper".into(),
            "juniper".into(),
            "queued".into(),
            5.0
        )
    );
    assert!(
        store
            .conn()
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let default: Option<String> = store
        .conn()
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('room_status') WHERE name = 'set_by_kind'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default, None);
    let member = Author::member("scribe", "wren");
    assert_eq!(
        store.register("lantern", "1.0", std::slice::from_ref(&member), 6.0),
        Ok(1)
    );
    assert!(store.is_member(&member).unwrap());
    assert_eq!(
        store.post("room", "node-a", &member, "new", 6.0),
        Ok(if version == 1 { 2 } else { 4 })
    );
    assert!(
        store
            .conn()
            .execute(
                "INSERT INTO outbox VALUES (8, 'room', 'node-a', 'Bad', 'wren', 'bad', 6.0)",
                []
            )
            .is_err()
    );
}

#[test]
fn an_earlier_fixed_list_store_migrates_to_generic_kinds() {
    check_earlier(2);
}

#[test]
fn an_earlier_version_1_fixed_list_store_migrates_to_generic_kinds() {
    check_earlier(1);
}

#[test]
fn fresh_and_migrated_schemas_match() {
    let fresh = Store::new("schema-fresh");
    let fresh = fresh.open();
    for version in [1, 2] {
        let fixture = Store::new("schema-earlier");
        earlier(&fixture, version);
        assert_eq!(schema(&fresh), schema(&fixture.open()));
    }
}

#[test]
fn author_kinds_are_checked_everywhere() {
    let fixture = Store::new("kinds");
    let mut store = fixture.room();
    assert_eq!(READ_MAX, 500);
    for kind in ["helper", "a", "a-b_c9", &"a".repeat(40)] {
        assert!(is_kind(kind));
        let by = Author::member(kind, "juniper");
        assert_eq!(
            store.register("lantern", "1", std::slice::from_ref(&by), 1.0),
            Ok(1)
        );
        assert_eq!(store.is_member(&by), Ok(true));
        assert!(store.post("room", "node-a", &by, "hi", 1.0).is_ok());
        assert!(
            store
                .set_status("room", kind, "ok", "node-a", &by, None, 1.0)
                .is_ok()
        );
        assert!(
            store
                .clear_status("room", kind, "node-a", &by, 1.0)
                .unwrap()
                .is_some()
        );
    }
    for kind in ["", "Helper", "1x", "a b", "a.b", "ä", &"a".repeat(41)] {
        assert!(!is_kind(kind));
        let by = Author::member(kind, "juniper");
        let expected = Err("not a member kind".to_owned());
        assert_eq!(
            store.post("room", "node-a", &by, "hi", 1.0).map(|_| ()),
            expected
        );
        assert_eq!(
            store
                .set_status("room", "key", "ok", "node-a", &by, None, 1.0)
                .map(|_| ()),
            expected
        );
        assert_eq!(
            store
                .clear_status("room", "key", "node-a", &by, 1.0)
                .map(|_| ()),
            expected
        );
        assert_eq!(
            store
                .register("lantern", "1", std::slice::from_ref(&by), 1.0)
                .map(|_| ()),
            expected
        );
        assert_eq!(store.is_member(&by).map(|_| ()), expected);
    }
    for kind in ["a-b", "a.b", "A", "", "1x", &"a".repeat(41)] {
        let valid = kind == "a-b";
        for sql in [
            "INSERT INTO messages (room, seq, author_host, author_kind, author, body, sent_at, kind) VALUES ('room', 999, 'node-a', ?1, 'wren', 'hi', 1.0, 'post')",
            "INSERT INTO room_status (room, key, line, set_by, set_at, set_by_kind) VALUES ('room', 'raw', 'hi', 'wren', 1.0, ?1)",
            "INSERT INTO outbox VALUES (999, 'room', 'node-a', ?1, 'wren', 'hi', 1.0)",
            "INSERT INTO members VALUES ('lantern', ?1, 'wren')",
        ] {
            store.conn().execute_batch("SAVEPOINT kind_case").unwrap();
            let result = store.conn().execute(sql, [kind]);
            if valid {
                assert_eq!(result.unwrap(), 1, "{kind}: {sql}");
            } else {
                let rusqlite::Error::SqliteFailure(error, _) = result.unwrap_err() else {
                    panic!("expected a constraint failure");
                };
                assert_eq!(
                    error.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                    "{kind}: {sql}"
                );
            }
            store
                .conn()
                .execute_batch("ROLLBACK TO kind_case; RELEASE kind_case")
                .unwrap();
        }
    }
    assert!(is_kind(HUMAN));
    assert!(human().is_human());
    assert!(!author().is_human());
}

#[test]
fn register_replaces_lists_per_app() {
    let fixture = Store::new("register");
    let mut store = fixture.open();
    let other = Author::member("scribe", "wren");
    assert_eq!(store.register("lantern", "1.0", &[author()], 1.0), Ok(1));
    assert_eq!(
        store.is_member(&Author::member("scribe", "juniper")),
        Ok(false)
    );
    assert_eq!(
        store.is_member(&Author::member("helper", "wren")),
        Ok(false)
    );
    assert_eq!(
        store.register("lantern-2", "2.0", &[other.clone(), author()], 1.0),
        Ok(2)
    );
    store.register("lantern", "1.1+test-2", &[], 2.0).unwrap();
    assert!(store.is_member(&author()).unwrap());
    store
        .register("lantern-2", "2.1", std::slice::from_ref(&other), 2.0)
        .unwrap();
    assert!(!store.is_member(&author()).unwrap());
    assert!(store.is_member(&other).unwrap());
    for who in ["sam", "ada", &"é".repeat(64)] {
        assert_eq!(store.is_member(&Author::human(who)), Ok(true));
    }
    for who in ["", &"a".repeat(65)] {
        assert_eq!(
            store.is_member(&Author::human(who)),
            Err("not a name".into())
        );
    }
    assert_eq!(
        store.register("lantern", "1", &[human()], 3.0),
        Err("not a member kind".into())
    );
    let many: Vec<_> = (0..MEMBERS_MAX)
        .map(|i| Author::member("helper", format!("juniper-{i}")))
        .collect();
    assert_eq!(MEMBERS_MAX, 256);
    assert_eq!(store.register("lantern", "3", &many, 3.0), Ok(256));
    let mut excess = many.clone();
    excess.push(Author::member("Bad", ""));
    assert_eq!(
        store.register("lantern", "4", &excess, 4.0),
        Err("too many members".into())
    );
    assert_eq!(
        store.register(
            "lantern",
            "4",
            &[author(), Author::member("helper", "")],
            4.0
        ),
        Err("not a name".into())
    );
    assert_eq!(
        store.register("lantern", "4", &[author()], f64::NAN),
        Err("not a time".into())
    );
    assert!(store.is_member(&many[255]).unwrap());
    assert!(!store.is_member(&author()).unwrap());
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT app_version, registered_at FROM apps WHERE app = 'lantern'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            )
            .unwrap(),
        ("3".into(), 3.0)
    );
    for app in ["", "a/b", "a b", "ä", &"a".repeat(65)] {
        assert_eq!(
            store.register(app, "1", &[], 1.0),
            Err("not an app name".into())
        );
    }
    for version in ["", "a b", "v/1", "ä", &"a".repeat(65)] {
        assert_eq!(
            store.register("lantern", version, &[], 1.0),
            Err("not a version".into())
        );
    }
    let raw = Author::member("helper", " jun\u{200B}iper\n");
    assert_eq!(
        store.register(
            "lantern",
            "4",
            &[raw.clone(), Author::member("helper", "jun iper")],
            4.0
        ),
        Ok(1)
    );
    assert_eq!(store.is_member(&raw), Ok(true));
    drop(store);
    let mut store = fixture.open();
    assert_eq!(store.is_member(&raw), Ok(true));
    assert_eq!(store.is_member(&other), Ok(true));
    store.create_room("room", "node-a", "Room", 1.0).unwrap();
    store.post("room", "node-a", &raw, "hi", 1.0).unwrap();
    assert_eq!(store.tail("room", 1).unwrap()[0].author.name, "jun iper");
}

#[test]
fn ownership_needs_kind_and_name() {
    let fixture = Store::new("kind-owner");
    let mut store = fixture.room();
    store
        .set_status("room", "key", "ok", "node-a", &author(), None, 1.0)
        .unwrap();
    for by in [
        Author::member("scribe", "juniper"),
        Author::member("helper", "wren"),
    ] {
        assert_eq!(
            store.set_status("room", "key", "bad", "node-a", &by, None, 2.0),
            Err(OWNED.into())
        );
        assert_eq!(
            store.clear_status("room", "key", "node-a", &by, 2.0),
            Err(OWNED.into())
        );
    }
    assert!(
        store
            .set_status("room", "key", "ok", "node-a", &author(), None, 2.0)
            .is_ok()
    );
    assert!(
        store
            .clear_status("room", "key", "node-a", &author(), 2.0)
            .unwrap()
            .is_some()
    );
    for who in ["sam", "ada"] {
        store
            .set_status("room", "key", "ok", "node-a", &author(), None, 3.0)
            .unwrap();
        assert!(
            store
                .set_status(
                    "room",
                    "key",
                    "ok",
                    "node-a",
                    &Author::human(who),
                    None,
                    4.0
                )
                .is_ok()
        );
        assert!(
            store
                .clear_status("room", "key", "node-a", &Author::human(who), 4.0)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn registration_rolls_back_a_failed_insert() {
    let fixture = Store::new("register-rollback");
    let mut store = fixture.open();
    store.register("lantern", "1", &[author()], 1.0).unwrap();
    store.conn().execute_batch("CREATE TRIGGER refuse_member BEFORE INSERT ON members WHEN NEW.name = 'wren' BEGIN SELECT RAISE(ABORT, 'refused'); END;").unwrap();
    assert_eq!(
        store.register("lantern", "2", &[Author::member("scribe", "wren")], 2.0),
        Err("the room store could not be written".into())
    );
    assert_eq!(store.is_member(&author()), Ok(true));
    assert_eq!(
        store.is_member(&Author::member("scribe", "wren")),
        Ok(false)
    );
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT app_version, registered_at FROM apps WHERE app = 'lantern'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            )
            .unwrap(),
        ("1".into(), 1.0)
    );
}

#[test]
fn private_directories_refuse_files_and_links_and_obey_recursion() {
    let fixture = Store::new("private-dir");
    let nested = fixture.0.join("parent/child");
    assert_eq!(
        crate::private_dir(&nested, false),
        Err("could not be created")
    );
    crate::private_dir(&nested, true).unwrap();
    assert_eq!(
        fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let file = fixture.0.join("file");
    fs::write(&file, "").unwrap();
    assert_eq!(crate::private_dir(&file, true), Err("not a directory"));
    let link = fixture.0.join("link");
    symlink(&nested, &link).unwrap();
    assert_eq!(crate::private_dir(&link, true), Err("not a directory"));
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
    crate::private_dir(&nested, false).unwrap();
    assert_eq!(
        fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn text_cleaning_preserves_helper_boundaries() {
    use crate::text::{clean, is_invisible, truncate};
    assert_eq!(truncate("héllo", 3), "hél…");
    assert_eq!(truncate("héllo", 5), "héllo");
    assert_eq!(truncate("", 0), "");
    assert_eq!(
        clean(" \0a\nb\tc\u{200B}d\u{FEFF} ", usize::MAX),
        "a\nb\tc d"
    );
    for (lo, hi) in [
        (0x200B, 0x200F),
        (0x2028, 0x202E),
        (0x2060, 0x2069),
        (0xFEFF, 0xFEFF),
    ] {
        for code in lo..=hi {
            assert!(is_invisible(char::from_u32(code).unwrap()));
        }
        assert!(!is_invisible(char::from_u32(lo - 1).unwrap()));
        assert!(!is_invisible(char::from_u32(hi + 1).unwrap()));
    }
}
