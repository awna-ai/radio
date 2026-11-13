use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::{
    EndpointAddr, EndpointId,
    endpoint::{
        AfterHandshakeOutcome, BeforeConnectOutcome, Connection, EndpointHooks, Side,
        WeakConnectionHandle,
    },
};

use tokio::time::Instant;

use crate::{
    endpoint::{ALPN, JOIN_ALPN},
    fleet_db::FleetDb,
    link::REFUSED,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Accepted,
    Refused,
    Unknown,
}

/// Never call `close_key` while holding a lock used by `standing` or `join_open`; apply the change, release the lock, then close the key.
pub trait Admission: Send + Sync + 'static {
    fn standing(&self, key: &EndpointId) -> Standing;
    fn join_open(&self) -> bool;
    fn fleet(&self) -> Option<String>;
    fn delivery_folder(&self, _app: &str) -> Option<std::path::PathBuf> {
        None
    }
}

impl Admission for Mutex<FleetDb> {
    fn delivery_folder(&self, app: &str) -> Option<std::path::PathBuf> {
        self.lock()
            .ok()
            .and_then(|db| db.delivery_folder(app).ok().flatten())
    }
    fn standing(&self, key: &iroh::EndpointId) -> Standing {
        self.lock()
            .ok()
            .and_then(|db| db.standing(key).ok())
            .unwrap_or(Standing::Refused)
    }
    fn join_open(&self) -> bool {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|now| {
                self.lock()
                    .ok()
                    .and_then(|db| db.join_open(now.as_secs_f64()).ok())
            })
            .unwrap_or(false)
    }
    fn fleet(&self) -> Option<String> {
        self.lock()
            .ok()
            .and_then(|db| db.fleet().ok().flatten())
            .map(|record| record.body.id)
    }
}

#[derive(Clone)]
pub struct Limits {
    pub file: Duration,
    pub deliveries: usize,
    pub connections: usize,
    pub per_key: usize,
    pub joins: usize,
    pub handshakes: usize,
    pub idle: Duration,
    pub frame: Duration,
    pub dial: Duration,
    pub handshake: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file: Duration::from_secs(120),
            deliveries: 2,
            connections: 32,
            per_key: 4,
            joins: 4,
            handshakes: 32,
            idle: Duration::from_secs(30),
            frame: Duration::from_secs(5),
            dial: Duration::from_secs(5),
            handshake: Duration::from_secs(5),
        }
    }
}

struct Live {
    key: EndpointId,
    join: bool,
    connection: WeakConnectionHandle,
    admitted_at: Instant,
}

pub struct Gate {
    pub(crate) admission: Arc<dyn Admission>,
    pub(crate) limits: Limits,
    live: Mutex<Vec<Live>>,
    pub(crate) deliveries: Arc<tokio::sync::Semaphore>,
}

impl fmt::Debug for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gate").finish_non_exhaustive()
    }
}

impl Gate {
    pub(crate) fn deliveries_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        self.deliveries.clone().try_acquire_owned()
    }

    pub fn new(admission: Arc<dyn Admission>, limits: Limits) -> Self {
        Self {
            admission,
            deliveries: Arc::new(tokio::sync::Semaphore::new(limits.deliveries)),
            limits,
            live: Mutex::new(Vec::new()),
        }
    }

    /// Release the admission lock before calling this, as for `close_key`.
    pub fn close_all(&self) {
        for entry in self.live.lock().unwrap().drain(..) {
            if let Some(connection) = entry.connection.upgrade() {
                connection.close(REFUSED.into(), &[]);
            }
        }
    }

    pub(crate) fn join_deadline(&self, connection: &Connection) -> Option<Instant> {
        self.live.lock().unwrap().iter().find_map(|entry| {
            entry.connection.upgrade().and_then(|live| {
                (entry.join && live.stable_id() == connection.stable_id())
                    .then_some(entry.admitted_at + self.limits.frame)
            })
        })
    }

    /// Never call while holding a lock used by your `standing` or `join_open`; apply the change, release the lock, then close the key.
    pub fn close_key(&self, key: &EndpointId) {
        let mut live = self.live.lock().unwrap();
        live.retain(|entry| {
            if entry.key == *key {
                if let Some(connection) = entry.connection.upgrade() {
                    connection.close(REFUSED.into(), &[]);
                }
                false
            } else {
                true
            }
        });
    }
}

impl EndpointHooks for Gate {
    async fn before_connect(&self, remote: &EndpointAddr, alpn: &[u8]) -> BeforeConnectOutcome {
        let standing = self.admission.standing(&remote.id);
        if (alpn == ALPN && standing == Standing::Accepted)
            || (alpn == JOIN_ALPN && standing != Standing::Refused)
        {
            BeforeConnectOutcome::Accept
        } else {
            BeforeConnectOutcome::Reject
        }
    }

    async fn after_handshake(&self, conn: &Connection) -> AfterHandshakeOutcome {
        let refuse = || AfterHandshakeOutcome::Reject {
            error_code: REFUSED.into(),
            reason: vec![],
        };
        let key = conn.remote_id();
        let mut live = self.live.lock().unwrap();
        let standing = self.admission.standing(&key);
        let join = match conn.alpn() {
            ALPN if standing == Standing::Accepted => false,
            JOIN_ALPN
                if standing != Standing::Refused
                    && (conn.side() == Side::Client || self.admission.join_open()) =>
            {
                true
            }
            _ => return refuse(),
        };
        live.retain(|entry| {
            entry
                .connection
                .upgrade()
                .is_some_and(|conn| conn.close_reason().is_none())
        });
        if live.len() >= self.limits.connections
            || live.iter().filter(|entry| entry.key == key).count() >= self.limits.per_key
            || (join && live.iter().filter(|entry| entry.join).count() >= self.limits.joins)
        {
            return refuse();
        }
        live.push(Live {
            key,
            join,
            connection: conn.weak_handle(),
            admitted_at: Instant::now(),
        });
        AfterHandshakeOutcome::Accept
    }
}

#[derive(Debug)]
pub(crate) struct Hooks(pub(crate) Arc<Gate>);

impl EndpointHooks for Hooks {
    async fn before_connect(&self, remote: &EndpointAddr, alpn: &[u8]) -> BeforeConnectOutcome {
        self.0.before_connect(remote, alpn).await
    }

    async fn after_handshake(&self, conn: &Connection) -> AfterHandshakeOutcome {
        self.0.after_handshake(conn).await
    }
}

#[cfg(test)]
#[path = "admit_tests.rs"]
mod tests;
