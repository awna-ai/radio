#[test]
fn transport_versions_and_features_are_pinned() {
    let lock =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let packages: Vec<_> = lock
        .split("[[package]]")
        .skip(1)
        .map(|block| {
            let field = |name: &str| {
                block
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix(&format!("{name} = \""))
                            .and_then(|value| value.strip_suffix('"'))
                    })
                    .unwrap()
            };
            (field("name"), field("version"))
        })
        .collect();
    for name in [
        "iroh",
        "iroh-base",
        "iroh-dns",
        "iroh-relay",
        "noq",
        "noq-proto",
        "noq-udp",
    ] {
        let versions: Vec<_> = packages
            .iter()
            .filter(|(package, _)| *package == name)
            .map(|(_, version)| *version)
            .collect();
        assert_eq!(versions, ["1.3.0"], "{name}");
    }
    let versions: Vec<_> = packages
        .iter()
        .filter(|(name, _)| *name == "rustls")
        .map(|(_, version)| *version)
        .collect();
    assert_eq!(
        versions,
        ["0.23.46"],
        "the ticket-cache behaviour max_tls_tickets(0) relies on was read at rustls 0.23.46"
    );
    assert!(
        !packages.iter().any(|(name, _)| *name == "portmapper"),
        "portmapper"
    );
}
