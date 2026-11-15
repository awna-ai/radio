//! `status.json`: the computers this Radio is connected to, written whole on each connect and close.
use crate::{EndpointId, admit::Admission};
use iroh::{
    TransportAddr,
    endpoint::{Connection, WeakConnectionHandle},
};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// The relay line reads this until Radio uses a relay.
pub const NO_RELAY: &str = "none";

/// The direct address a connection sends on, when its selected path is one.
pub(crate) fn direct_addr(conn: &Connection) -> Option<SocketAddr> {
    let paths = conn.paths();
    let path = paths.iter().find(|path| path.is_selected())?;
    match path.remote_addr() {
        TransportAddr::Ip(addr) => Some(*addr),
        _ => None,
    }
}

pub(crate) struct Status {
    path: PathBuf,
    admission: Arc<dyn Admission>,
    conns: Mutex<Vec<(EndpointId, WeakConnectionHandle)>>,
}

impl Status {
    pub(crate) fn new(path: PathBuf, admission: Arc<dyn Admission>) -> Arc<Self> {
        Arc::new(Self {
            path,
            admission,
            conns: Mutex::new(Vec::new()),
        })
    }

    /// Lists a member's connection now and drops it from the file once it closes.
    pub(crate) fn track(self: &Arc<Self>, key: EndpointId, conn: &Connection) {
        let weak = conn.weak_handle();
        self.conns.lock().unwrap().push((key, weak.clone()));
        let status = self.clone();
        tokio::spawn(async move {
            status.write();
            weak.closed().await;
            status.write();
        });
    }

    /// Best effort: a file that cannot be written leaves the last one in place.
    pub(crate) fn write(&self) {
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|(_, weak)| {
            weak.upgrade()
                .is_some_and(|conn| conn.close_reason().is_none())
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |at| at.as_secs());
        let mut seen = HashSet::new();
        let mut computers = Vec::new();
        for (key, weak) in conns.iter() {
            let Some(conn) = weak.upgrade() else {
                continue;
            };
            let paths = conn.paths();
            let Some(path) = paths.iter().find(|path| path.is_selected()) else {
                continue;
            };
            let TransportAddr::Ip(addr) = path.remote_addr() else {
                continue;
            };
            if !seen.insert(*key) {
                continue;
            }
            computers.push(serde_json::json!({
                "name": self.admission.name(key),
                "key": key.to_string(),
                "path": "direct",
                "addr": addr.to_string(),
                "rtt_ms": path.rtt().as_millis() as u64,
                "last_seen": now,
            }));
        }
        let body = serde_json::json!({ "computers": computers, "relay": NO_RELAY });
        let Ok(mut text) = serde_json::to_vec_pretty(&body) else {
            return;
        };
        text.push(b'\n');
        let temp = self.path.with_extension("json.tmp");
        let _ = fs::remove_file(&temp);
        let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)
        else {
            return;
        };
        if file.write_all(&text).is_err() || fs::rename(&temp, &self.path).is_err() {
            let _ = fs::remove_file(&temp);
        }
    }
}
