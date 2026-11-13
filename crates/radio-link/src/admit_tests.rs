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
