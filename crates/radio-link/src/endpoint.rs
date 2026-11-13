use std::net::{IpAddr, SocketAddr};

pub const ALPN: &[u8] = b"radio/1";
const ADDRESS: &str = "the link address is not usable";

pub async fn bind(
    key: iroh::SecretKey,
    addrs: &[SocketAddr],
) -> Result<iroh::Endpoint, &'static str> {
    if addrs.is_empty()
        || addrs.iter().any(|addr| {
            let ip = addr.ip();
            ip.is_unspecified()
                || ip.is_multicast()
                || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
        })
    {
        return Err(ADDRESS);
    }
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
        .max_tls_tickets(0)
        .clear_ip_transports();
    for addr in addrs {
        builder = builder.bind_addr(*addr).map_err(|_| ADDRESS)?;
    }
    builder
        .bind()
        .await
        .map_err(|_| "the link port could not be bound")
}
