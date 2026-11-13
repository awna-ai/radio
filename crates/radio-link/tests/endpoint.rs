use radio_link::{
    SecretKey,
    endpoint::{self, ALPN},
};
use std::net::{SocketAddr, UdpSocket};

#[tokio::test(flavor = "current_thread")]
async fn refuses_unconfigured_and_nonlocal_addresses() {
    for addrs in [
        vec![],
        vec!["0.0.0.0:0".parse().unwrap()],
        vec!["[::]:0".parse().unwrap()],
        vec!["224.0.0.1:0".parse().unwrap()],
        vec!["255.255.255.255:0".parse().unwrap()],
        vec!["[ff02::1]:0".parse().unwrap()],
        vec!["127.0.0.1:0".parse().unwrap(), "0.0.0.0:0".parse().unwrap()],
    ] {
        assert_eq!(
            endpoint::bind(SecretKey::generate(), &addrs).await.err(),
            Some("the link address is not usable")
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn binds_only_configured_ipv4_without_relays_or_lookup() {
    assert_eq!(ALPN, b"radio/1");
    let key = SecretKey::generate();
    let id = key.public();
    let endpoint = endpoint::bind(key, &["127.0.0.1:0".parse().unwrap()])
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
        endpoint::bind(SecretKey::generate(), &[socket.local_addr().unwrap()])
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
    let endpoint = endpoint::bind(SecretKey::generate(), &[addr])
        .await
        .unwrap();
    assert!(endpoint.bound_sockets().contains(&addr));
    endpoint.close().await;
}
