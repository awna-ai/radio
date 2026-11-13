use radio_link::{
    SecretKey,
    admit::{Admission, Gate, Limits, Standing},
    endpoint::{self, ALPN},
};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::Arc,
};

struct Empty;
impl Admission for Empty {
    fn standing(&self, _: &iroh::EndpointId) -> Standing {
        Standing::Unknown
    }
    fn join_open(&self) -> bool {
        false
    }
    fn fleet(&self) -> Option<String> {
        None
    }
}
fn gate() -> Arc<Gate> {
    Arc::new(Gate::new(Arc::new(Empty), Limits::default()))
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_unconfigured_and_nonlocal_addresses() {
    let mut refused = Vec::new();
    for addrs in [
        vec![],
        vec!["0.0.0.0:0".parse().unwrap()],
        vec!["[::]:0".parse().unwrap()],
        vec!["224.0.0.1:0".parse().unwrap()],
        vec!["255.255.255.255:0".parse().unwrap()],
        vec!["[ff02::1]:0".parse().unwrap()],
        vec!["[::ffff:0.0.0.0]:0".parse().unwrap()],
        vec![
            "127.0.0.1:1000".parse().unwrap(),
            "127.0.0.2:1000".parse().unwrap(),
        ],
        vec![
            "127.0.0.1:1000".parse().unwrap(),
            "[::1]:1001".parse().unwrap(),
        ],
        vec!["127.0.0.1:0".parse().unwrap(), "0.0.0.0:0".parse().unwrap()],
    ] {
        let result = endpoint::bind(SecretKey::generate(), &addrs, gate()).await;
        let error = match result {
            Ok(endpoint) => {
                endpoint.close().await;
                None
            }
            Err(error) => Some(error),
        };
        refused.push((addrs, error));
    }
    for (addrs, error) in &refused {
        eprintln!("{addrs:?}: {error:?}");
    }
    assert!(
        refused
            .iter()
            .all(|(_, error)| *error == Some("the link address is not usable"))
    );
}
#[tokio::test(flavor = "current_thread")]
async fn binds_only_configured_ipv4_without_relays_or_lookup() {
    assert_eq!(ALPN, b"radio/1");
    let key = SecretKey::generate();
    let id = key.public();
    let endpoint = endpoint::bind(key, &["127.0.0.1:0".parse().unwrap()], gate())
        .await
        .unwrap();
    assert_eq!(endpoint.id(), id);
    let sockets = endpoint.bound_sockets();
    assert!(!sockets.is_empty());
    assert!(
        sockets
            .iter()
            .all(|addr| addr.ip() == std::net::Ipv4Addr::LOCALHOST)
    );
    assert_eq!(endpoint.addr().relay_urls().count(), 0);
    assert!(endpoint.address_lookup().unwrap().is_empty());
    endpoint.close().await;
}
#[tokio::test(flavor = "current_thread")]
async fn refuses_an_occupied_port() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    assert_eq!(
        endpoint::bind(
            SecretKey::generate(),
            &[socket.local_addr().unwrap()],
            gate()
        )
        .await
        .err(),
        Some("the link port could not be bound")
    );
}
#[tokio::test(flavor = "current_thread")]
async fn keeps_the_configured_fixed_port() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr: SocketAddr = socket.local_addr().unwrap();
    drop(socket);
    let endpoint = endpoint::bind(SecretKey::generate(), &[addr], gate())
        .await
        .unwrap();
    assert!(endpoint.bound_sockets().contains(&addr));
    endpoint.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn binds_both_families_on_one_fixed_port() {
    if let Err(error) = UdpSocket::bind("[::1]:0") {
        eprintln!("skipping dual-family bind: plain IPv6 loopback UDP bind failed: {error}");
        return;
    }
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let mut addrs: Vec<SocketAddr> = vec![
        SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
    ];
    let endpoint = endpoint::bind(SecretKey::generate(), &addrs, gate())
        .await
        .unwrap();
    let mut sockets = endpoint.bound_sockets();
    addrs.sort();
    sockets.sort();
    assert_eq!(sockets, addrs);
    endpoint.close().await;
}
