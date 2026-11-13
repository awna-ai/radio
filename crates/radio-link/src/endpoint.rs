use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use iroh::endpoint::QuicTransportConfig;

use crate::admit::{Gate, Hooks};

pub const ALPN: &[u8] = b"radio/1";
pub const JOIN_ALPN: &[u8] = b"radio-join/1";
pub const PORT: u16 = 7341;
pub const NO_OWN_ADDRESS: &str = "this computer has no link address";
pub const NO_ADDRESS: &str = "no address is known for that computer";
pub const STREAMS: u32 = 16;
const ADDRESS: &str = "the link address is not usable";

pub(crate) fn usable(addrs: &[SocketAddr]) -> Result<Vec<SocketAddr>, &'static str> {
    let mut canonical = Vec::with_capacity(addrs.len());
    let mut ipv4 = false;
    let mut ipv6 = false;
    let Some(first) = addrs.first() else {
        return Err(NO_OWN_ADDRESS);
    };
    for addr in addrs {
        let ip = addr.ip().to_canonical();
        let family = if ip.is_ipv4() { &mut ipv4 } else { &mut ipv6 };
        if *family || addr.port() != first.port() || !usable_ip(&ip) {
            return Err(ADDRESS);
        }
        *family = true;
        let mut addr = *addr;
        addr.set_ip(ip);
        canonical.push(addr);
    }
    Ok(canonical)
}

pub(crate) fn usable_ip(ip: &IpAddr) -> bool {
    let ip = ip.to_canonical();
    !ip.is_unspecified() && !ip.is_multicast() && !matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
}

pub fn link_ip(ip: &IpAddr) -> bool {
    let ip = ip.to_canonical();
    usable_ip(&ip) && !matches!(ip, IpAddr::V6(ip) if ip.segments()[0] & 0xffc0 == 0xfe80)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_addresses_need_no_interface_scope() {
        for address in [
            "127.0.0.1",
            "10.0.0.2",
            "100.64.0.7",
            "::1",
            "fd00::2",
            "fec0::1",
            "fe7f::1",
            "::ffff:10.0.0.2",
        ] {
            assert!(link_ip(&address.parse().unwrap()), "{address}");
        }
        for address in [
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "255.255.255.255",
            "ff02::1",
            "fe80::1",
            "febf::1",
        ] {
            assert!(!link_ip(&address.parse().unwrap()), "{address}");
        }
    }
}
