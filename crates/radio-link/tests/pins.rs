fn packages(lock: &str) -> Vec<(&str, &str, Option<&str>)> {
    lock.split("[[package]]")
        .skip(1)
        .map(|block| {
            let field = |name: &str| {
                block.lines().find_map(|line| {
                    line.strip_prefix(&format!("{name} = \""))
                        .and_then(|value| value.strip_suffix('"'))
                })
            };
            (
                field("name").unwrap(),
                field("version").unwrap(),
                field("checksum"),
            )
        })
        .collect()
}

#[test]
fn transport_versions_and_features_are_pinned() {
    let lock =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let packages = packages(&lock);
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
            .filter(|(package, _, _)| *package == name)
            .map(|(_, version, _)| *version)
            .collect();
        assert_eq!(versions, ["1.3.0"], "{name}");
    }
    let versions: Vec<_> = packages
        .iter()
        .filter(|(name, _, _)| *name == "rustls")
        .map(|(_, version, _)| *version)
        .collect();
    assert_eq!(
        versions,
        ["0.23.46"],
        "the ticket-cache behaviour max_tls_tickets(0) relies on was read at rustls 0.23.46"
    );
    assert!(
        !packages.iter().any(|(name, _, _)| *name == "portmapper"),
        "portmapper"
    );
}

#[test]
fn the_fuzz_lockfile_agrees_with_the_workspace() {
    let root =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let fuzz = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/Cargo.lock"
    ))
    .unwrap();
    let root = packages(&root);
    for (name, version, checksum) in packages(&fuzz) {
        if root.iter().any(|(package, _, _)| *package == name) {
            assert!(
                root.contains(&(name, version, checksum)),
                "{name} {version} has a different version or checksum in the fuzz lockfile"
            );
        }
    }
}
