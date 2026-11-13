fn packages(lock: &str) -> Vec<(&str, &str, Option<&str>, Option<&str>)> {
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
                field("source"),
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
            .filter(|(package, _, _, _)| *package == name)
            .map(|(_, version, _, _)| *version)
            .collect();
        assert_eq!(versions, ["1.3.0"], "{name}");
    }
    let versions: Vec<_> = packages
        .iter()
        .filter(|(name, _, _, _)| *name == "rustls")
        .map(|(_, version, _, _)| *version)
        .collect();
    assert_eq!(
        versions,
        ["0.23.46"],
        "the ticket-cache behaviour max_tls_tickets(0) relies on was read at rustls 0.23.46"
    );
    assert!(
        !packages.iter().any(|(name, _, _, _)| *name == "portmapper"),
        "portmapper"
    );
}

fn lock_disagreements(root: &str, fuzz: &str) -> Vec<String> {
    let root = packages(root);
    let fuzz = packages(fuzz);
    let mut disagreements = Vec::new();
    let shared: std::collections::BTreeSet<_> = fuzz
        .iter()
        .filter(|(name, _, _, _)| root.iter().any(|(package, _, _, _)| package == name))
        .map(|(name, _, _, _)| *name)
        .collect();
    if shared.is_empty() {
        disagreements.push("the lockfiles hold no package names in common".into());
    }
    const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
    let mut reviewed: std::collections::BTreeSet<_> = [
        (
            "arbitrary",
            "1.5.0",
            Some(REGISTRY),
            Some("3bc62ac97cc33321f50863d514c3bc38a453947a8f9e781137e47c7401020aed"),
        ),
        (
            "jobserver",
            "0.1.35",
            Some(REGISTRY),
            Some("1c00acbd29eabad4a2392fa0e921c874934dbbf4194312ad20f04a0ed67a3cb3"),
        ),
        (
            "libfuzzer-sys",
            "0.4.13",
            Some(REGISTRY),
            Some("a9fd2f41a1cba099f79a0b6b6c35656cf7c03351a7bae8ff0f28f25270f929d2"),
        ),
        ("radio-fuzz", "0.0.0", None, None),
    ]
    .into();
    for package @ (name, version, _, _) in fuzz {
        if shared.contains(name) {
            if !root.contains(&package) {
                disagreements.push(format!(
                    "{name} {version} has a different version, source or checksum in the fuzz lockfile"
                ));
            }
        } else if !reviewed.remove(&package) {
            disagreements.push(format!(
                "a package only the fuzz lockfile holds is not the one its intake reviewed: {package:?}"
            ));
        }
    }
    for package in reviewed {
        disagreements.push(format!(
            "a package only the fuzz lockfile holds is not the one its intake reviewed: {package:?}"
        ));
    }
    disagreements
}

#[test]
fn the_fuzz_lockfile_agrees_with_the_workspace() {
    let root = include_str!("../../../Cargo.lock");
    let fuzz = include_str!("../../../fuzz/Cargo.lock");
    let disagreements = lock_disagreements(root, fuzz);
    assert!(disagreements.is_empty(), "{disagreements:?}");
}

#[test]
fn the_fuzz_lockfile_check_catches_drift() {
    let root = include_str!("../../../Cargo.lock");
    let fuzz = include_str!("../../../fuzz/Cargo.lock");
    assert!(lock_disagreements(root, fuzz).is_empty());
    let shared = packages(root)
        .into_iter()
        .find(|package @ (_, _, _, checksum)| {
            checksum.is_some() && packages(fuzz).contains(package)
        })
        .unwrap();
    let alter = |name: &str, field: &str, value: &str| {
        let block = fuzz
            .split("[[package]]")
            .find(|block| {
                block
                    .lines()
                    .any(|line| line == format!("name = \"{name}\""))
            })
            .unwrap();
        let old = block
            .lines()
            .find(|line| line.starts_with(&format!("{field} = ")))
            .unwrap();
        fuzz.replacen(
            block,
            &block.replacen(old, &format!("{field} = \"{value}\""), 1),
            1,
        )
    };
    let drifted = [
        alter(shared.0, "version", "0.0.0"),
        alter(shared.0, "checksum", &"0".repeat(64)),
        alter("libfuzzer-sys", "version", "0.4.14"),
        alter("libfuzzer-sys", "checksum", &"0".repeat(64)),
        alter("libfuzzer-sys", "source", "git+https://example.invalid/x#0"),
        fuzz.replacen(
            "name = \"radio-link\"",
            "name = \"radio-link\"\nsource = \"git+https://example.invalid/radio#0\"",
            1,
        ),
        format!("{fuzz}\n[[package]]\nname = \"cedar\"\nversion = \"0.1.0\"\n"),
    ];
    for (case, fuzz) in drifted.iter().enumerate() {
        assert!(
            !lock_disagreements(root, fuzz).is_empty(),
            "drift case {case}"
        );
    }
}
