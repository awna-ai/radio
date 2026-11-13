use super::*;

#[test]
fn defaults_bound_connections_and_deadlines() {
    let limits = Limits::default();
    assert_eq!(limits.connections, 32);
    assert_eq!(limits.per_key, 4);
    assert_eq!(limits.joins, 4);
    assert_eq!(limits.handshakes, 32);
    assert_eq!(limits.idle, Duration::from_secs(30));
    assert_eq!(limits.frame, Duration::from_secs(5));
    assert_eq!(limits.dial, Duration::from_secs(5));
    assert_eq!(limits.handshake, Duration::from_secs(5));
}

struct Book(Standing);
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
