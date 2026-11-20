use super::*;

#[test]
fn defaults_bound_connections_and_deadlines() {
    let limits = Limits::default();
    assert_eq!(limits.connections, 32);
    assert_eq!(limits.dialed, 64);
    assert_eq!(limits.per_key, 4);
    assert_eq!(limits.joins, 4);
    assert_eq!(limits.handshakes, 32);
    assert_eq!(limits.idle, Duration::from_secs(660));
    assert_eq!(limits.unused, Duration::from_secs(600));
    assert_eq!(limits.heartbeat, Duration::from_secs(15));
    assert_eq!(limits.failed, Duration::from_secs(45));
    assert_eq!(limits.backoff_min, Duration::from_secs(1));
    assert_eq!(limits.backoff_max, Duration::from_secs(60));
    assert_eq!(limits.frame, Duration::from_secs(5));
    assert_eq!(limits.dial, Duration::from_secs(5));
    assert_eq!(limits.handshake, Duration::from_secs(5));
}

struct Book(Standing);

#[test]
fn gossip_handshakes_only_keep_audited_accepted_members() {
    let key = iroh::SecretKey::generate().public();
    for (state, standing, expected) in [
        ("accepted", Standing::Accepted, true),
        ("pending", Standing::Unknown, false),
        ("blocked", Standing::Refused, false),
        ("unknown", Standing::Unknown, false),
    ] {
        let gate = Gate::new(Arc::new(Book(standing)), Limits::default());
        assert_eq!(
            gate.member_protocol(b"radio-gossip/1", &key, standing),
            expected,
            "{state}"
        );
    }
}

#[tokio::test]
async fn gossip_dials_only_accepted_members() {
    let remote = EndpointAddr::from(iroh::SecretKey::generate().public());
    for (state, standing, expected) in [
        ("accepted", Standing::Accepted, true),
        ("pending", Standing::Unknown, false),
        ("blocked", Standing::Refused, false),
        ("unknown", Standing::Unknown, false),
    ] {
        let gate = Gate::new(Arc::new(Book(standing)), Limits::default());
        assert_eq!(
            matches!(
                gate.before_connect(&remote, b"radio-gossip/1").await,
                BeforeConnectOutcome::Accept
            ),
            expected,
            "{state}"
        );
    }
}
impl Admission for Book {
    fn standing(&self, _: &EndpointId) -> Standing {
        self.0
    }
    fn join_open(&self) -> bool {
        false
    }
    fn fleet(&self) -> Option<String> {
        None
    }
}

#[tokio::test(flavor = "current_thread")]
async fn outgoing_admission_checks_standing_and_alpn() {
    let remote = EndpointAddr::from(iroh::SecretKey::generate().public());
    for (standing, outcomes) in [
        (Standing::Accepted, [true, true, false]),
        (Standing::Refused, [false, false, false]),
        (Standing::Unknown, [false, true, false]),
    ] {
        let gate = Gate::new(Arc::new(Book(standing)), Limits::default());
        for (alpn, expected) in [ALPN, JOIN_ALPN, b"radio/2"].into_iter().zip(outcomes) {
            let accepted = matches!(
                gate.before_connect(&remote, alpn).await,
                BeforeConnectOutcome::Accept
            );
            assert_eq!(accepted, expected);
        }
    }
}

#[test]
fn debug_hides_membership() {
    let gate = Gate::new(Arc::new(Book(Standing::Accepted)), Limits::default());
    assert_eq!(format!("{gate:?}"), "Gate { .. }");
}

#[test]
fn delivery_space_includes_reservations_in_other_folders() {
    let path = std::env::temp_dir().join(format!("radio-gate-space-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let mut db = FleetDb::open(&path.join("fleet.db")).unwrap();
    db.set_delivery_folder("notes", &path.join("inbox"))
        .unwrap();
    let folder = db.delivery_folder("notes").unwrap().unwrap();
    let dir = crate::delivery::open_folder(&folder).unwrap();
    let available = crate::delivery::available(&dir).unwrap();
    let gate = Gate::new(Arc::new(Book(Standing::Accepted)), Limits::default());
    // A reservation in a different folder exhausts this volume's available space.
    gate.files
        .lock()
        .unwrap()
        .folders
        .insert((folder.dev, folder.ino.wrapping_add(1)), (1, available));
    assert_eq!(
        gate.reserve_delivery(iroh::SecretKey::generate().public(), &folder, "one.txt", 1)
            .err(),
        Some(crate::wire::SPACE)
    );
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn delivery_reservations_bound_keys_slots_and_folder_bytes_and_drop_releases_them() {
    let path = std::env::temp_dir().join(format!("radio-gate-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let mut db = FleetDb::open(&path.join("fleet.db")).unwrap();
    db.set_delivery_folder("notes", &path.join("inbox"))
        .unwrap();
    let folder = db.delivery_folder("notes").unwrap().unwrap();
    let gate = Gate::new(
        Arc::new(Book(Standing::Accepted)),
        Limits {
            deliveries: 2,
            ..Limits::default()
        },
    );
    let a = iroh::SecretKey::generate().public();
    let b = iroh::SecretKey::generate().public();
    let c = iroh::SecretKey::generate().public();
    let (_, first) = gate.reserve_delivery(a, &folder, "one.txt", 3).unwrap();
    assert_eq!(
        gate.reserve_delivery(a, &folder, "two.txt", 1).err(),
        Some(crate::wire::BUSY)
    );
    let (_, second) = gate.reserve_delivery(b, &folder, "two.txt", 4).unwrap();
    assert_eq!(
        gate.reserve_delivery(c, &folder, "three.txt", 1).err(),
        Some(crate::wire::BUSY)
    );
    assert_eq!(
        gate.files.lock().unwrap().folders[&(folder.dev, folder.ino)],
        (2, 7)
    );
    drop(first);
    let (_, third) = gate.reserve_delivery(a, &folder, "one.txt", 5).unwrap();
    drop(second);
    drop(third);
    assert!(gate.files.lock().unwrap().keys.is_empty());
    assert!(gate.files.lock().unwrap().folders.is_empty());
    assert_eq!(gate.deliveries.available_permits(), 2);
    let (_, reservation) = gate.reserve_delivery(a, &folder, "one.txt", 1).unwrap();
    let dir = crate::delivery::prepare(&folder, "one.txt").unwrap();
    assert_eq!(
        crate::delivery::write_open(&dir, "one.txt", |_| Err("stopped")),
        Err("stopped")
    );
    drop(reservation);
    assert!(gate.reserve_delivery(a, &folder, "one.txt", 1).is_ok());
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test(start_paused = true)]
async fn nonmembers_are_audited_once_without_another_standing_lookup() {
    struct Audit(std::sync::Mutex<Vec<String>>);
    impl crate::handler::Handlers for Audit {
        fn has_handler(&self, _app: &str) -> bool {
            false
        }
        fn run(&self, _envelope: &crate::handler::Envelope<'_>) -> crate::handler::Ran {
            crate::handler::Ran::NoHandler
        }
        fn refused(&self, _key: &EndpointId, name: Option<&str>, app: &str, reason: &str) {
            assert_eq!(name, None);
            assert_eq!(app, "");
            self.0.lock().unwrap().push(reason.into());
        }
    }
    let audit = Arc::new(Audit(std::sync::Mutex::new(Vec::new())));
    let gate = Gate::with_handlers(
        Arc::new(Book(Standing::Accepted)),
        Limits::default(),
        audit.clone(),
    );
    let key = crate::SecretKey::generate().public();
    assert!(gate.audited_member(&key, Standing::Accepted));
    assert!(audit.0.lock().unwrap().is_empty());
    for standing in [Standing::Unknown, Standing::Refused] {
        assert!(!gate.audited_member(&key, standing));
        assert_eq!(*audit.0.lock().unwrap(), vec!["not_member"]);
    }
    // Stream refusals and handshake refusals share this one budget.
    gate.messages.refused(
        &crate::handler::Envelope {
            app: "",
            from: key,
            from_name: None,
            id: "",
            body: "",
        },
        "not_member",
    );
    assert_eq!(*audit.0.lock().unwrap(), vec!["not_member"]);
    // Accepted members' other refusal reasons remain visible even during a flood.
    for _ in 0..100 {
        gate.messages.audit(&key, None, "", "clock");
    }
    assert_eq!(audit.0.lock().unwrap().len(), 101);
}

#[tokio::test(start_paused = true)]
async fn nonmember_audit_is_capped_per_key_and_over_a_rolling_minute() {
    struct Audit(std::sync::Mutex<usize>);
    impl crate::handler::Handlers for Audit {
        fn has_handler(&self, _app: &str) -> bool {
            false
        }
        fn run(&self, _envelope: &crate::handler::Envelope<'_>) -> crate::handler::Ran {
            crate::handler::Ran::NoHandler
        }
        fn refused(&self, _key: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
            assert_eq!(reason, "not_member");
            *self.0.lock().unwrap() += 1;
        }
    }
    let audit = Arc::new(Audit(std::sync::Mutex::new(0)));
    let gate = Gate::with_handlers(
        Arc::new(Book(Standing::Unknown)),
        Limits::default(),
        audit.clone(),
    );
    let key = crate::SecretKey::generate().public();
    assert!(!gate.audited_member(&key, Standing::Unknown));
    for _ in 1..100 {
        assert!(!gate.audited_member(&crate::SecretKey::generate().public(), Standing::Unknown));
    }
    assert_eq!(*audit.0.lock().unwrap(), 60);
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(!gate.audited_member(&key, Standing::Unknown));
    assert_eq!(*audit.0.lock().unwrap(), 60);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(!gate.audited_member(&key, Standing::Unknown));
    assert_eq!(*audit.0.lock().unwrap(), 61);
    // Expiry is per entry, rather than resetting the counter on a wall-clock minute.
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..59 {
        assert!(!gate.audited_member(&crate::SecretKey::generate().public(), Standing::Unknown));
    }
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(!gate.audited_member(&key, Standing::Unknown));
    assert!(!gate.audited_member(&crate::SecretKey::generate().public(), Standing::Unknown));
    assert_eq!(*audit.0.lock().unwrap(), 121);
}
