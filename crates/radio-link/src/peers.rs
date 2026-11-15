//! Dialed connections and the in-memory lists that keep them open.
use crate::{EndpointId, admit::Limits};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    net::SocketAddr,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    runtime::Handle,
    sync::watch,
    time::{Instant, sleep_until, timeout},
};

pub(crate) trait Connector: Send + Sync + 'static {
    type Conn: Clone + Send + Sync + 'static;
    fn connect(
        &self,
        id: EndpointId,
        addrs: Option<Vec<SocketAddr>>,
    ) -> impl Future<Output = Result<Self::Conn, &'static str>> + Send;
    fn heartbeat(&self, conn: &Self::Conn)
    -> impl Future<Output = Result<(), &'static str>> + Send;
    fn closed(&self, conn: &Self::Conn) -> impl Future<Output = ()> + Send;
    fn is_live(&self, conn: &Self::Conn) -> bool;
    fn close_idle(&self, conn: &Self::Conn);
    fn name(&self, id: &EndpointId) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptState {
    Connected,
    Dialing,
    Failed,
}
impl KeptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Dialing => "dialing",
            Self::Failed => "failed",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptPeer {
    pub id: EndpointId,
    pub name: Option<String>,
    pub state: KeptState,
}

struct Activity {
    users: usize,
    last_used: Instant,
}
/// Every returned peer holds a guard; clones of a guard share one use.
pub(crate) struct Use {
    activity: Arc<Mutex<Activity>>,
    changed: watch::Sender<()>,
}
impl Drop for Use {
    fn drop(&mut self) {
        let mut activity = self.activity.lock().unwrap();
        activity.users -= 1;
        if activity.users == 0 {
            activity.last_used = Instant::now();
        }
        self.changed.send_replace(());
    }
}
struct Entry<T> {
    conn: T,
    activity: Arc<Mutex<Activity>>,
    connected: Instant,
}
struct Keeper {
    token: Arc<()>,
    state: KeptState,
}
type DialResult<T> = Result<Arc<Entry<T>>, &'static str>;
struct State<T> {
    conns: HashMap<EndpointId, Arc<Entry<T>>>,
    flights: HashMap<EndpointId, watch::Receiver<Option<DialResult<T>>>>,
    apps: HashMap<String, Vec<EndpointId>>,
    keepers: HashMap<EndpointId, Keeper>,
}
pub(crate) struct Peers<C: Connector> {
    connector: Arc<C>,
    limits: Limits,
    runtime: Handle,
    state: Mutex<State<C::Conn>>,
    changed: watch::Sender<()>,
}
impl<C: Connector> Peers<C> {
    pub(crate) fn new(connector: C, limits: Limits, runtime: Handle) -> Arc<Self> {
        Arc::new(Self {
            connector: Arc::new(connector),
            limits,
            runtime,
            state: Mutex::new(State {
                conns: HashMap::new(),
                flights: HashMap::new(),
                apps: HashMap::new(),
                keepers: HashMap::new(),
            }),
            changed: watch::channel(()).0,
        })
    }
    async fn entry(
        self: &Arc<Self>,
        id: EndpointId,
        addrs: Option<Vec<SocketAddr>>,
    ) -> DialResult<C::Conn> {
        let mut receiver = {
            let mut state = self.state.lock().unwrap();
            if let Some(entry) = state.conns.get(&id) {
                if self.connector.is_live(&entry.conn) {
                    return Ok(entry.clone());
                }
                state.conns.remove(&id);
                self.changed.send_replace(());
            }
            if let Some(receiver) = state.flights.get(&id) {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                state.flights.insert(id, receiver.clone());
                let pool = self.clone();
                // The shared dial outlives any individual caller that cancels its wait.
                self.runtime.spawn(async move {
                    let result = pool.connector.connect(id, addrs).await.map(|conn| {
                        Arc::new(Entry {
                            conn,
                            activity: Arc::new(Mutex::new(Activity {
                                users: 0,
                                last_used: Instant::now(),
                            })),
                            connected: Instant::now(),
                        })
                    });
                    {
                        let mut state = pool.state.lock().unwrap();
                        if let Ok(entry) = &result {
                            state.conns.insert(id, entry.clone());
                            let weak = Arc::downgrade(&pool);
                            let entry = entry.clone();
                            let connector = pool.connector.clone();
                            let limits = pool.limits.clone();
                            let changes = pool.changed.subscribe();
                            pool.runtime
                                .spawn(mind(weak, id, entry, connector, limits, changes));
                        }
                        // Publish while the slot is locked: every current waiter gets this result.
                        sender.send_replace(Some(result));
                        state.flights.remove(&id);
                    }
                    pool.changed.send_replace(());
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err("the computer could not be reached");
            }
        }
    }
    pub(crate) async fn get(
        self: &Arc<Self>,
        id: EndpointId,
        addrs: Option<Vec<SocketAddr>>,
    ) -> Result<(C::Conn, Arc<Use>), &'static str> {
        loop {
            let entry = self.entry(id, addrs.clone()).await?;
            let _state = self.state.lock().unwrap();
            if !self.connector.is_live(&entry.conn) {
                continue;
            }
            entry.activity.lock().unwrap().users += 1;
            self.changed.send_replace(());
            return Ok((
                entry.conn.clone(),
                Arc::new(Use {
                    activity: entry.activity.clone(),
                    changed: self.changed.clone(),
                }),
            ));
        }
    }
    pub(crate) fn keep(self: &Arc<Self>, app: &str, ids: Vec<EndpointId>) {
        let mut seen = HashSet::new();
        let ids: Vec<_> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
        let mut state = self.state.lock().unwrap();
        if ids.is_empty() {
            state.apps.remove(app);
        } else {
            state.apps.insert(app.into(), ids);
        }
        let union: HashSet<_> = state.apps.values().flatten().copied().collect();
        let removed: Vec<_> = state
            .keepers
            .keys()
            .filter(|id| !union.contains(*id))
            .copied()
            .collect();
        for id in removed {
            state.keepers.remove(&id);
            if let Some(entry) = state.conns.get(&id) {
                entry.activity.lock().unwrap().last_used = Instant::now();
            }
        }
        for id in union {
            if state.keepers.contains_key(&id) {
                continue;
            }
            let token = Arc::new(());
            let status = if state
                .conns
                .get(&id)
                .is_some_and(|entry| self.connector.is_live(&entry.conn))
            {
                KeptState::Connected
            } else {
                KeptState::Dialing
            };
            state.keepers.insert(
                id,
                Keeper {
                    token: token.clone(),
                    state: status,
                },
            );
            self.runtime.spawn(keep(
                Arc::downgrade(self),
                id,
                token,
                self.changed.subscribe(),
                self.limits.clone(),
                status == KeptState::Connected,
            ));
        }
        self.changed.send_replace(());
    }
    pub(crate) fn kept(&self, app: &str) -> Vec<KeptPeer> {
        let state = self.state.lock().unwrap();
        state
            .apps
            .get(app)
            .into_iter()
            .flatten()
            .map(|id| KeptPeer {
                id: *id,
                name: self.connector.name(id),
                state: state.keepers[id].state,
            })
            .collect()
    }
    fn keeper_state(&self, id: EndpointId, token: &Arc<()>, status: KeptState) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(keeper) = state
            .keepers
            .get_mut(&id)
            .filter(|k| Arc::ptr_eq(&k.token, token))
        else {
            return false;
        };
        keeper.state = status;
        true
    }
    fn remove(&self, id: EndpointId, entry: &Arc<Entry<C::Conn>>, failed: bool) {
        let mut state = self.state.lock().unwrap();
        if state
            .conns
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            state.conns.remove(&id);
            if failed && let Some(keeper) = state.keepers.get_mut(&id) {
                keeper.state = KeptState::Failed;
            }
            self.changed.send_replace(());
        }
    }
}

async fn keep<C: Connector>(
    pool: Weak<Peers<C>>,
    id: EndpointId,
    token: Arc<()>,
    mut changes: watch::Receiver<()>,
    limits: Limits,
    initially_connected: bool,
) {
    let mut backoff = limits.backoff_min;
    let mut connected = initially_connected;
    // A peer that closes each connection right after its handshake keeps growing the backoff;
    // only a connection that stays up for a heartbeat interval resets it.
    let mut up_since = initially_connected.then(Instant::now);
    let mut retry_at = None;
    loop {
        let Some(owner) = pool.upgrade() else {
            return;
        };
        let (wanted, live) = {
            let mut state = owner.state.lock().unwrap();
            let live = state
                .conns
                .get(&id)
                .is_some_and(|e| owner.connector.is_live(&e.conn));
            let wanted = state
                .keepers
                .get(&id)
                .is_some_and(|k| Arc::ptr_eq(&k.token, &token));
            if live && wanted {
                state.keepers.get_mut(&id).unwrap().state = KeptState::Connected;
            }
            (wanted, live)
        };
        if !wanted {
            return;
        }
        if live {
            connected = true;
            up_since.get_or_insert_with(Instant::now);
            retry_at = None;
            drop(owner);
            if changes.changed().await.is_err() {
                return;
            }
            continue;
        }
        if connected {
            owner.keeper_state(id, &token, KeptState::Failed);
            if up_since.is_some_and(|at| Instant::now() >= at + limits.heartbeat) {
                backoff = limits.backoff_min;
            }
            up_since = None;
            retry_at = Some(Instant::now() + backoff);
            backoff = (backoff * 2).min(limits.backoff_max);
            connected = false;
        }
        if let Some(at) = retry_at
            && Instant::now() < at
        {
            drop(owner);
            tokio::select! {
                _ = sleep_until(at) => {},
                result = changes.changed() => {
                    if result.is_err() {
                        return;
                    }
                }
            }
            continue;
        }
        if !owner.keeper_state(id, &token, KeptState::Dialing) {
            return;
        }
        // Leaving the union cancels this wait, while a shared in-flight dial may finish unused.
        let dial = owner.entry(id, None);
        tokio::pin!(dial);
        let result = loop {
            tokio::select! {
                result = &mut dial => break result,
                result = changes.changed() => {
                    if result.is_err()
                        || !owner.state.lock().unwrap().keepers.get(&id)
                            .is_some_and(|k| Arc::ptr_eq(&k.token, &token))
                    {
                        return;
                    }
                }
            }
        };
        match result {
            Ok(_) => {
                connected = true;
                up_since = Some(Instant::now());
                retry_at = None;
            }
            Err(_) => {
                owner.keeper_state(id, &token, KeptState::Failed);
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(limits.backoff_max);
            }
        }
    }
}

async fn mind<C: Connector>(
    pool: Weak<Peers<C>>,
    id: EndpointId,
    entry: Arc<Entry<C::Conn>>,
    connector: Arc<C>,
    limits: Limits,
    mut changes: watch::Receiver<()>,
) {
    let mut last_ok = entry.connected;
    let mut next_beat = Instant::now() + limits.heartbeat;
    let mut was_kept = false;
    let mut heartbeat = None;
    loop {
        let Some(owner) = pool.upgrade() else {
            return;
        };
        let kept = owner.state.lock().unwrap().keepers.contains_key(&id);
        if !connector.is_live(&entry.conn) {
            owner.remove(id, &entry, false);
            return;
        }
        if kept && !was_kept {
            last_ok = Instant::now();
            next_beat = Instant::now() + limits.heartbeat;
        }
        if !kept {
            heartbeat = None;
        }
        was_kept = kept;
        let (unused_at, in_use) = {
            let a = entry.activity.lock().unwrap();
            (a.last_used + limits.unused, a.users > 0)
        };
        drop(owner);
        tokio::select! {
            biased;
            _ = connector.closed(&entry.conn) => {
                if let Some(owner) = pool.upgrade() {
                    owner.remove(id, &entry, false);
                }
                return;
            }
            _ = sleep_until(last_ok+limits.failed), if kept => {
                if let Some(owner) = pool.upgrade() {
                    let state = owner.state.lock().unwrap();
                    if state.keepers.contains_key(&id) {
                        connector.close_idle(&entry.conn);
                        drop(state);
                        owner.remove(id, &entry, true);
                        return;
                    }
                } else {
                    return;
                }
            }
            _ = sleep_until(unused_at), if !kept && !in_use => {
                // Recheck under the pool and activity locks so a concurrent keep/use wins.
                if let Some(owner) = pool.upgrade() {
                    let state = owner.state.lock().unwrap();
                    let activity = entry.activity.lock().unwrap();
                    if !state.keepers.contains_key(&id)
                        && activity.users == 0
                        && Instant::now() >= activity.last_used + limits.unused
                    {
                        connector.close_idle(&entry.conn);
                        drop(activity);
                        drop(state);
                        owner.remove(id, &entry, false);
                        return;
                    }
                } else {
                    return;
                }
            }
            result = async { heartbeat.as_mut().unwrap().await }, if heartbeat.is_some() => {
                if matches!(result, Ok(Ok(()))) {
                    last_ok = Instant::now();
                }
                heartbeat = None;
            }
            _ = sleep_until(next_beat), if kept && heartbeat.is_none() => {
                next_beat += limits.heartbeat;
                let connector = connector.clone();
                let conn = entry.conn.clone();
                let wait = limits.heartbeat;
                heartbeat = Some(Box::pin(async move {
                    timeout(wait, connector.heartbeat(&conn)).await
                }));
            }
            result = changes.changed() => {
                if result.is_err() {
                    return;
                }
            }
        }
    }
}
#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
