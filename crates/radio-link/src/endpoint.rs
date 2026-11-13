use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use iroh::endpoint::QuicTransportConfig;

use crate::admit::{Gate, Hooks};

pub const ALPN: &[u8] = b"radio/1";
pub const JOIN_ALPN: &[u8] = b"radio-join/1";
pub const STREAMS: u32 = 16;
const ADDRESS: &str = "the link address is not usable";

pub(crate) fn usable(addrs: &[SocketAddr]) -> Result<Vec<SocketAddr>, &'static str> {
    let mut canonical = Vec::with_capacity(addrs.len());
    let mut ipv4 = false;
    let mut ipv6 = false;
    let Some(first) = addrs.first() else {
        return Err(ADDRESS);
    };
    for addr in addrs {
        let ip = addr.ip().to_canonical();
        let family = if ip.is_ipv4() { &mut ipv4 } else { &mut ipv6 };
        if *family
            || addr.port() != first.port()
            || ip.is_unspecified()
            || ip.is_multicast()
            || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(ADDRESS);
        }
        *family = true;
        let mut addr = *addr;
        addr.set_ip(ip);
        canonical.push(addr);
    }
    Ok(canonical)
}

pub async fn bind(
    key: iroh::SecretKey,
    addrs: &[SocketAddr],
    gate: Arc<Gate>,
) -> Result<iroh::Endpoint, &'static str> {
    let canonical = usable(addrs)?;
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec(), JOIN_ALPN.to_vec()])
        .transport_config(
            QuicTransportConfig::builder()
                .max_concurrent_bidi_streams(STREAMS.into())
                .max_concurrent_uni_streams(0u32.into())
                .datagram_receive_buffer_size(None)
                .build(),
        )
        .hooks(Hooks(gate))
        .relay_mode(iroh::RelayMode::Disabled)
        .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
        .max_tls_tickets(0)
        .clear_ip_transports();
    for addr in canonical {
        builder = builder.bind_addr(addr).map_err(|_| ADDRESS)?;
    }
    builder
        .bind()
        .await
        .map_err(|_| "the link port could not be bound")
}
