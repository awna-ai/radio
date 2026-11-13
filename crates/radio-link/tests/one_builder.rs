use std::{fs, path::Path};

fn scan(dir: &Path, builder: &Path, own: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() == "target" {
            continue;
        }
        if path.is_dir() {
            scan(&path, builder, own);
        } else if path.extension().is_some_and(|ext| ext == "rs") && path != own {
            let source = fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains(concat!("into_", "0rtt")),
                "{}",
                path.display()
            );
            if path != builder {
                for forbidden in [
                    concat!("Endpoint::", "builder"),
                    concat!("Endpoint::", "bind("),
                    concat!("Endpoint::", "empty_builder"),
                    concat!("Builder::", "empty("),
                    concat!("presets", "::"),
                ] {
                    assert!(
                        !source.contains(forbidden),
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
    );
}
