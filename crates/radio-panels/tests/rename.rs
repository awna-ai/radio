//! The fixtures are an earlier copy's, with one field's name changed to `member`.
//! Digests of that copy with the word masked prove every other byte is the same.
//! A fixture added later has no earlier copy: list it with no offsets and its own file's SHA-256.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Deserialize)]
struct Manifest {
    files: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    path: String,
    masked_sha256: String,
    renamed_at: Vec<usize>,
}

fn manifest() -> Manifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rename_manifest.json");
    serde_json::from_slice(&fs::read(path).expect("rename manifest"))
        .expect("valid rename manifest")
}

#[test]
fn every_fixture_is_in_the_rename_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut expected: BTreeSet<String> = fs::read_dir(root.join("tests/fixtures"))
        .expect("fixture directory")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            path.strip_prefix(root)
                .expect("fixture under crate root")
                .to_str()
                .expect("UTF-8 fixture path")
                .to_owned()
        })
        .collect();
    expected.insert("tests/reference_probes.json".into());
    let entries = manifest().files;
    let actual: BTreeSet<_> = entries.iter().map(|entry| entry.path.clone()).collect();
    assert_eq!(entries.len(), actual.len(), "duplicate manifest paths");
    assert_eq!(actual.len(), 67, "manifest must cover 67 files");
    assert_eq!(actual, expected, "manifest must cover every fixture");
}

#[test]
fn fixtures_differ_from_their_source_only_by_the_renamed_field() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for entry in manifest().files {
        let check = || -> Result<(), String> {
            let bytes = fs::read(root.join(&entry.path)).map_err(|error| error.to_string())?;
            if bytes.contains(&0) {
                return Err("fixture contains a NUL byte".into());
            }
            let mut masked = Vec::with_capacity(bytes.len());
            let mut cursor = 0;
            for &offset in &entry.renamed_at {
                if offset < cursor {
                    return Err(format!(
                        "renamed spans are unordered or overlap at {offset}"
                    ));
                }
                let end = offset.checked_add(6).ok_or("renamed offset overflow")?;
                if bytes.get(offset..end) != Some(b"member".as_slice()) {
                    return Err(format!("renamed span at {offset} must equal member"));
                }
                masked.extend_from_slice(&bytes[cursor..offset]);
                masked.push(0);
                cursor = end;
            }
            masked.extend_from_slice(&bytes[cursor..]);
            let digest: String = Sha256::digest(&masked)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if digest != entry.masked_sha256 {
                return Err(format!("masked SHA-256 differs: {digest}"));
            }
            Ok(())
        };
        if let Err(error) = check() {
            failures.push(format!("{}: {error}", entry.path));
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
