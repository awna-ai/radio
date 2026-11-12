mod common;

use common::assert_vector;
use radio_panels::evaluate_fixture;
use serde_json::Value;
use std::{fs, path::Path};

#[test]
fn all_json_vectors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<_> = fs::read_dir(root)
        .expect("fixture directory")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 66);
    let mut failures = Vec::new();
    for path in paths {
        let name = path
            .file_stem()
            .expect("fixture stem")
            .to_str()
            .expect("UTF-8 stem");
        let check = || -> Result<(), String> {
            let fixture: Value =
                serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if fixture["name"] != name {
                return Err("name does not equal file stem".into());
            }
            let actual =
                evaluate_fixture(&fixture).map_err(|e| format!("evaluate_fixture: {e}"))?;
            let expected = fixture.get("expect").ok_or("missing expect")?;
            if fixture["type"] == "rounds" {
                assert_vector(&actual["rounds"], &expected["rounds"], "rounds", false)?;
                assert_vector(&actual["result"], &expected["result"], "result", true)
            } else {
                assert_vector(&actual, expected, "", false)
            }
        };
        if let Err(error) = check() {
            failures.push(format!("{name} ({}): {error}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
