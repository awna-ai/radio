mod common;

use common::*;
use iroh::endpoint::{ConnectOptions, ConnectionError, PortmapperConfig, presets};
use radio_link::{
    SecretKey,
    admit::{Limits, Standing},
    endpoint::ALPN,
    frame::{self, CONTROL_CAP},
    link::{Event, REFUSED},
    wire::{Answer, Request},
};

async fn hostile() -> iroh::Endpoint {
    bounded(
        iroh::Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
            .unwrap()
            .bind(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn an_unknown_alpn_fails_the_handshake() {
    let book = Book::new();
    let (server, mut events) = bind(book.clone(), Limits::default()).await;
    let client = hostile().await;
    book.set(client.id(), Standing::Accepted);
    assert!(
        bounded(client.connect(address(&server), b"radio/2"))
            .await
            .is_err()
    );
    empty(&mut events).await;
    bounded(client.close()).await;
    bounded(server.close()).await;
}

#[tokio::test]
async fn removed_member_cannot_replay_a_hello_in_early_data() {
    let book = Book::new();
    let (server, mut events) = bind(book.clone(), Limits::default()).await;
    let client = hostile().await;
    book.set(client.id(), Standing::Accepted);
    let connection = bounded(client.connect(address(&server), ALPN))
        .await
        .unwrap();
    let (mut send, mut recv) = bounded(connection.open_bi()).await.unwrap();
    bounded(frame::write(
        &mut send,
        &Request::Hello { hello: hello() },
        CONTROL_CAP,
    ))
    .await
    .unwrap();
    send.finish().unwrap();
    let answer: Answer = bounded(frame::read(&mut recv, CONTROL_CAP)).await.unwrap();
    assert!(matches!(answer, Answer::Hello { .. }));
    match bounded(events.recv()).await.unwrap() {
        Event::Hello { from, .. } => assert_eq!(from, client.id()),
        _ => panic!("expected hello"),
    }
    connection.close(0u32.into(), &[]);
    drop(send);
    drop(recv);
    drop(connection);
    book.set(client.id(), Standing::Refused);
    let connecting =
        bounded(client.connect_with_opts(address(&server), ALPN, ConnectOptions::new()))
            .await
            .unwrap();
    let early = match connecting.into_0rtt() {
        Ok(connection) => connection,
        Err(_) => panic!("the server issued no ticket"),
    };
    let (mut send, _recv) = bounded(early.open_bi()).await.unwrap();
    bounded(frame::write(
        &mut send,
        &Request::Hello { hello: hello() },
        CONTROL_CAP,
    ))
    .await
    .unwrap();
    send.finish().unwrap();
    match bounded(early.closed()).await {
        ConnectionError::ApplicationClosed(close) => {
            assert_eq!(close.error_code, REFUSED.into());
            assert!(close.reason.is_empty());
        }
        other => panic!("expected an application close, got {other:?}"),
    }
    empty(&mut events).await;
    bounded(client.close()).await;
    bounded(server.close()).await;
}
