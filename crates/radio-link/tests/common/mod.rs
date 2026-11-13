#![allow(dead_code)]

use radio_link::{
    EndpointId, SecretKey,
    admit::{Admission, Limits, Standing},
    link::{Event, Link, Local},
    wire::{Hello, Protocol},
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};

pub const WAIT: Duration = Duration::from_secs(5);
pub const FLEET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub struct Book {
    pub standing: Mutex<HashMap<EndpointId, Standing>>,
    pub join: AtomicBool,
    pub fleet: Option<String>,
}

impl Book {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            standing: Mutex::new(HashMap::new()),
            join: AtomicBool::new(false),
            fleet: Some(FLEET.into()),
        })
    }

    pub fn set(&self, key: EndpointId, standing: Standing) {
        self.standing.lock().unwrap().insert(key, standing);
    }

    pub fn set_join(&self, open: bool) {
        self.join.store(open, Ordering::SeqCst);
    }
}

impl Admission for Book {
    fn standing(&self, key: &EndpointId) -> Standing {
        self.standing
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap_or(Standing::Unknown)
    }

    fn join_open(&self) -> bool {
        self.join.load(Ordering::SeqCst)
    }

    fn fleet(&self) -> Option<String> {
        self.fleet.clone()
    }
}

pub async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(WAIT, future)
        .await
        .expect("the operation timed out")
}

pub async fn bind(book: Arc<Book>, limits: Limits) -> (Link, mpsc::Receiver<Event>) {
    bind_key(SecretKey::generate(), book, limits).await
}

pub async fn bind_key(
    key: SecretKey,
    book: Arc<Book>,
    limits: Limits,
) -> (Link, mpsc::Receiver<Event>) {
    let link = bounded(Link::bind(
        key,
        &["127.0.0.1:0".parse().unwrap()],
        book,
        limits,
        Local {
            name: "garden".into(),
            sha: SHA.into(),
        },
    ))
    .await
    .unwrap();
    let (sender, receiver) = mpsc::channel(64);
    link.serve(sender);
    (link, receiver)
}

pub fn hello() -> Hello {
    Hello {
        protocol: Protocol { major: 1, minor: 0 },
        fleet: FLEET.into(),
        sha: SHA.into(),
        arch: "aarch64".into(),
        name: "juniper".into(),
    }
}

pub async fn empty(events: &mut mpsc::Receiver<Event>) {
    assert!(
        timeout(Duration::from_millis(100), events.recv())
            .await
            .is_err(),
        "an unexpected event arrived"
    );
}

pub fn address(link: &Link) -> iroh::EndpointAddr {
    iroh::EndpointAddr::from_parts(
        link.id(),
        link.addrs().into_iter().map(iroh::TransportAddr::Ip),
    )
}

pub async fn closed(connection: &iroh::endpoint::Connection, code: u32) {
    match bounded(connection.closed()).await {
        iroh::endpoint::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(close.error_code, code.into());
            assert!(close.reason.is_empty());
        }
        other => panic!("expected an application close, got {other:?}"),
    }
}

pub struct Raw {
    endpoint: iroh::Endpoint,
}

impl Raw {
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub async fn connect(&self, server: &Link, alpn: &[u8]) -> iroh::endpoint::Connection {
        bounded(self.endpoint.connect(address(server), alpn))
            .await
            .unwrap()
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

pub async fn raw_bind(book: Arc<Book>, limits: Limits) -> Raw {
    Raw {
        endpoint: bounded(radio_link::endpoint::bind(
            SecretKey::generate(),
            &["127.0.0.1:0".parse().unwrap()],
            Arc::new(radio_link::admit::Gate::new(book, limits)),
        ))
        .await
        .unwrap(),
    }
}
