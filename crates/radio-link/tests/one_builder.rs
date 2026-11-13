use std::{fs, path::Path};

fn contains_forbidden(source: &str, forbidden: &str) -> bool {
    source.match_indices(forbidden).any(|(offset, _)| {
        forbidden != concat!("Builder::", "new(")
            || source[..offset]
                .chars()
                .next_back()
                .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
    })
}

fn scan(dir: &Path, builder: &Path, own: &Path, manifest: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() == "target" {
            continue;
        }
        if path.is_dir() {
            scan(&path, builder, own, manifest);
        } else if path.file_name().unwrap() == "Cargo.toml" && path != manifest {
            let source = fs::read_to_string(&path).unwrap();
            let mut dependencies = false;
            for line in source.lines().map(str::trim) {
                if line.starts_with('[') {
                    dependencies = line.contains("dependencies");
                } else if dependencies {
                    assert!(
                        line.split(['=', '.']).next().unwrap().trim() != "iroh",
                        "{}: transport dependency outside its crate",
                        path.display()
                    );
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") && path != own {
            let source = fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains(concat!("into_", "0rtt")),
                "{}",
                path.display()
            );
            if path == builder {
                assert_eq!(
                    source.matches(concat!("Endpoint::", "builder")).count(),
                    1,
                    "{}",
                    path.display()
                );
                for required in [
                    concat!(".max_tls_", "tickets(0)"),
                    concat!("RelayMode::", "Disabled"),
                    concat!("PortmapperConfig::", "Disabled"),
                    concat!(".clear_ip_", "transports()"),
                ] {
                    assert!(
                        source.contains(required),
                        "{}: missing {required}",
                        path.display()
                    );
                }
                for forbidden in [
                    concat!("address_", "lookup("),
                    concat!("proxy", "_"),
                    concat!("key", "log("),
                    concat!("bind_addr_", "with_opts"),
                    concat!(".pre", "set("),
                    concat!("Builder::", "new("),
                ] {
                    assert!(
                        !contains_forbidden(&source, forbidden),
                        "{}: {forbidden}",
                        path.display()
                    );
                }
            } else {
                for forbidden in [
                    concat!("Endpoint::", "builder"),
                    concat!("Endpoint::", "bind("),
                    concat!("Endpoint::", "empty_builder"),
                    concat!("Builder::", "empty("),
                    concat!("presets", "::"),
                    concat!("Builder::", "new("),
                    concat!(".pre", "set("),
                ] {
                    assert!(
                        !contains_forbidden(&source, forbidden),
                        "{}: {forbidden}",
                        path.display()
                    );
                }
            }
        }
    }
}
#[test]
fn only_one_endpoint_builder_and_no_early_data() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    scan(
        crates,
        &crates.join("radio-link/src/endpoint.rs"),
        &crates.join("radio-link/tests/one_builder.rs"),
        &crates.join("radio-link/Cargo.toml"),
    );
}
