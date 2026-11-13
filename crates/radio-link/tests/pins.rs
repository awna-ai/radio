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
    let mut names = std::collections::BTreeSet::new();
    for package @ (name, version, source, checksum) in fuzz {
        if shared.contains(name) {
            if !root.contains(&package) {
                disagreements.push(format!(
                    "{name} {version} has a different version, source or checksum in the fuzz lockfile"
                ));
            }
        } else {
            names.insert(name);
            let provenance_matches = if name == "radio-fuzz" {
                source.is_none() && checksum.is_none()
            } else {
                source == Some("registry+https://github.com/rust-lang/crates.io-index")
                    && checksum.is_some()
            };
            if !provenance_matches {
                disagreements.push(format!(
                    "{name} has unexpected source or checksum in the fuzz lockfile"
                ));
            }
        }
    }
    if names != ["arbitrary", "jobserver", "libfuzzer-sys", "radio-fuzz"].into() {
        disagreements.push(format!(
            "a package only the fuzz lockfile holds needs its own intake: {names:?}"
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
