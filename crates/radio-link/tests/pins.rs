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
    for name in [
        "portmapper",
        "iroh-mdns-address-lookup",
        "pkarr",
        "hickory-resolver",
    ] {
        assert!(
            !packages.iter().any(|(package, _)| *package == name),
            "{name}"
        );
    }
}
