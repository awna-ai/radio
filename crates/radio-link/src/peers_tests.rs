use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Conn(Arc<Connection>);
struct Connection {
    live: AtomicBool,
    idle: AtomicBool,
    closed: Notify,
}
#[derive(Default)]
struct Fake {
    connects: AtomicUsize,
    beats: AtomicUsize,
    fail: AtomicBool,
    silent: AtomicBool,
    bad_beat: AtomicBool,
    pause_connect: AtomicBool,
    resume_connect: Notify,
    conns: Mutex<Vec<Conn>>,
}
impl Connector for Arc<Fake> {
    type Conn = Conn;
    async fn connect(
        &self,
        _: EndpointId,
        _: Option<Vec<SocketAddr>>,
    ) -> Result<Conn, &'static str> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        if self.pause_connect.load(Ordering::SeqCst) {
            self.resume_connect.notified().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err("the computer could not be reached");
        }
        let conn = Conn(Arc::new(Connection {
            live: AtomicBool::new(true),
            idle: AtomicBool::new(false),
            closed: Notify::new(),
        }));
        self.conns.lock().unwrap().push(conn.clone());
        Ok(conn)
    }
    async fn heartbeat(&self, _: &Conn) -> Result<(), &'static str> {
        self.beats.fetch_add(1, Ordering::SeqCst);
        if self.bad_beat.load(Ordering::SeqCst) {
            Err("the computer did not answer")
        } else if self.silent.load(Ordering::SeqCst) {
            std::future::pending().await
        } else {
            Ok(())
        }
    }
    async fn closed(&self, conn: &Conn) {
        let wait = conn.0.closed.notified();
        if self.is_live(conn) {
            wait.await;
        }
    }
    fn is_live(&self, conn: &Conn) -> bool {
        conn.0.live.load(Ordering::SeqCst)
    }
    fn close_idle(&self, conn: &Conn) {
        conn.0.idle.store(true, Ordering::SeqCst);
        close(conn);
    }
    fn name(&self, _: &EndpointId) -> Option<String> {
        Some("birch".into())
    }
}
fn close(conn: &Conn) {
    conn.0.live.store(false, Ordering::SeqCst);
    conn.0.closed.notify_waiters();
}
fn setup() -> (Arc<Peers<Arc<Fake>>>, Arc<Fake>, EndpointId) {
    let fake = Arc::new(Fake::default());
    (
        Peers::new(fake.clone(), Limits::default(), Handle::current()),
        fake,
        crate::SecretKey::generate().public(),
    )
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
async fn advance(seconds: u64) {
    for _ in 0..seconds {
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
    }
}
fn state(pool: &Peers<Arc<Fake>>) -> KeptState {
    pool.kept("notes")[0].state
}

#[tokio::test(start_paused = true)]
async fn a_second_use_reuses_the_first_connection() {
    let (pool, fake, id) = setup();
    drop(pool.get(id, Some(vec![])).await.unwrap());
    drop(pool.get(id, Some(vec![])).await.unwrap());
    assert_eq!(fake.connects.load(Ordering::SeqCst), 1);
    let (pool, fake, id) = setup();
    let (a, b) = tokio::join!(pool.get(id, None), pool.get(id, None));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(fake.connects.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn a_failed_dial_leaves_nothing_and_the_next_use_dials_again() {
    let (pool, fake, id) = setup();
    fake.fail.store(true, Ordering::SeqCst);
    let (a, b) = tokio::join!(pool.get(id, None), pool.get(id, None));
    assert_eq!(a.err(), Some("the computer could not be reached"));
    assert!(b.is_err());
    assert_eq!(fake.connects.load(Ordering::SeqCst), 1);
    fake.fail.store(false, Ordering::SeqCst);
    assert!(pool.get(id, None).await.is_ok());
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
}
#[tokio::test(start_paused = true)]
async fn an_unused_connection_closes_at_ten_minutes_and_not_before() {
    let (pool, fake, id) = setup();
    let (conn, guard) = pool.get(id, None).await.unwrap();
    drop(guard);
    advance(599).await;
    assert!(fake.is_live(&conn));
    advance(2).await;
    assert!(!fake.is_live(&conn));
    assert!(conn.0.idle.load(Ordering::SeqCst));
    let (conn, guard) = pool.get(id, None).await.unwrap();
    advance(700).await;
    assert!(fake.is_live(&conn));
    drop(guard);
    advance(599).await;
    assert!(fake.is_live(&conn));
    advance(2).await;
    assert!(!fake.is_live(&conn));
}
#[tokio::test(start_paused = true)]
async fn a_peer_no_one_uses_or_keeps_holds_no_connection() {
    let (pool, fake, id) = setup();
    let kept = crate::SecretKey::generate().public();
    let untouched = crate::SecretKey::generate().public();
    drop(pool.get(id, None).await.unwrap());
    pool.keep("notes", vec![kept]);
    settle().await;
    assert_ne!(untouched, id);
    assert_ne!(untouched, kept);
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
}
#[tokio::test(start_paused = true)]
async fn a_kept_peer_heartbeats_every_fifteen_seconds() {
    let (pool, fake, id) = setup();
    pool.keep("notes", vec![id]);
    settle().await;
    advance(46).await;
    assert_eq!(fake.beats.load(Ordering::SeqCst), 3);
    assert_eq!(state(&pool), KeptState::Connected);
}
#[tokio::test(start_paused = true)]
async fn a_silent_kept_peer_fails_at_forty_five_seconds() {
    let (pool, fake, id) = setup();
    fake.silent.store(true, Ordering::SeqCst);
    pool.keep("notes", vec![id]);
    settle().await;
    let start = Instant::now();
    advance(44).await;
    assert_eq!(state(&pool), KeptState::Connected);
    advance(1).await;
    assert_eq!(Instant::now() - start, Duration::from_secs(45));
    assert_eq!(state(&pool), KeptState::Failed);
    assert!(!fake.is_live(&fake.conns.lock().unwrap()[0]));
}
#[tokio::test(start_paused = true)]
async fn a_kept_peer_is_redialed_with_backoff() {
    let (pool, fake, id) = setup();
    pool.keep("notes", vec![id]);
    settle().await;
    assert_eq!(fake.conns.lock().unwrap().len(), 1);
    fake.fail.store(true, Ordering::SeqCst);
    close(&fake.conns.lock().unwrap()[0]);
    settle().await;
    let mut previous = 0;
    for (i, at) in [1, 3, 7, 15, 31, 63, 123, 183].into_iter().enumerate() {
        advance(at - previous - 1).await;
        assert_eq!(fake.connects.load(Ordering::SeqCst), i + 1);
        advance(1).await;
        assert_eq!(fake.connects.load(Ordering::SeqCst), i + 2);
        assert_eq!(state(&pool), KeptState::Failed);
        previous = at;
    }
    fake.fail.store(false, Ordering::SeqCst);
    advance(60).await;
    assert_eq!(state(&pool), KeptState::Connected);
    close(&fake.conns.lock().unwrap()[1]);
    settle().await;
    let n = fake.connects.load(Ordering::SeqCst);
    advance(1).await;
    assert_eq!(fake.connects.load(Ordering::SeqCst), n + 1);
}
#[tokio::test(start_paused = true)]
async fn keeping_none_lets_them_go() {
    let (pool, fake, id) = setup();
    pool.keep("notes", vec![id]);
    settle().await;
    advance(50).await;
    assert_eq!(fake.conns.lock().unwrap().len(), 1);
    pool.keep("notes", vec![]);
    settle().await;
    assert!(pool.kept("notes").is_empty());
    let conn = fake.conns.lock().unwrap()[0].clone();
    advance(599).await;
    assert!(fake.is_live(&conn));
    advance(2).await;
    assert!(!fake.is_live(&conn));
    advance(120).await;
    assert_eq!(fake.connects.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn kept_lists_are_per_app_and_the_pool_keeps_their_union() {
    let (pool, fake, id) = setup();
    let other = crate::SecretKey::generate().public();
    pool.keep("notes", vec![other, id, id]);
    pool.keep("garden", vec![id]);
    settle().await;
    assert_eq!(
        pool.kept("notes").iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![other, id]
    );
    assert_eq!(pool.kept("garden")[0].name.as_deref(), Some("birch"));
    pool.keep("notes", vec![]);
    advance(601).await;
    assert_eq!(pool.kept("garden")[0].state, KeptState::Connected);
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn keeping_an_existing_connection_still_waits_before_redial() {
    let (pool, fake, id) = setup();
    let (conn, guard) = pool.get(id, None).await.unwrap();
    drop(guard);
    pool.keep("notes", vec![id]);
    close(&conn);
    settle().await;
    assert_eq!(fake.connects.load(Ordering::SeqCst), 1);
    advance(1).await;
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
    assert_eq!(state(&pool), KeptState::Connected);
}

#[tokio::test(start_paused = true)]
async fn closed_connections_are_replaced_on_the_next_use() {
    let (pool, fake, id) = setup();
    let (conn, guard) = pool.get(id, None).await.unwrap();
    drop(guard);
    close(&conn);
    let (fresh, _guard) = pool.get(id, None).await.unwrap();
    assert!(fake.is_live(&fresh));
    assert!(!Arc::ptr_eq(&conn.0, &fresh.0));
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn only_the_last_use_starts_the_unused_deadline() {
    let (pool, fake, id) = setup();
    let (conn, first) = pool.get(id, None).await.unwrap();
    let (_, last) = pool.get(id, None).await.unwrap();
    drop(first);
    advance(700).await;
    assert!(fake.is_live(&conn));
    drop(last);
    advance(599).await;
    assert!(fake.is_live(&conn));
    advance(2).await;
    assert!(!fake.is_live(&conn));
}

#[tokio::test(start_paused = true)]
async fn unsuccessful_heartbeats_do_not_refresh_the_failure_deadline() {
    let (pool, fake, id) = setup();
    fake.bad_beat.store(true, Ordering::SeqCst);
    pool.keep("notes", vec![id]);
    settle().await;
    advance(44).await;
    assert_eq!(state(&pool), KeptState::Connected);
    advance(1).await;
    assert_eq!(state(&pool), KeptState::Failed);
    assert_eq!(fake.beats.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn an_on_demand_redial_wakes_the_keeper_when_it_removes_a_closed_connection() {
    let (pool, fake, id) = setup();
    pool.keep("notes", vec![id]);
    settle().await;
    assert_eq!(state(&pool), KeptState::Connected);
    fake.pause_connect.store(true, Ordering::SeqCst);
    close(&fake.conns.lock().unwrap()[0]);
    let pending = pool.get(id, None);
    tokio::pin!(pending);
    // Poll the on-demand caller before the old connection's minder can remove it.
    tokio::select! {
        biased;
        _ = &mut pending => panic!("the new dial must still be pending"),
        _ = std::future::ready(()) => {},
    }
    settle().await;
    advance(1).await;
    assert_eq!(state(&pool), KeptState::Dialing);
    assert_eq!(fake.connects.load(Ordering::SeqCst), 2);
    fake.resume_connect.notify_one();
    assert!(pending.await.is_ok());
    settle().await;
    assert_eq!(state(&pool), KeptState::Connected);
}
