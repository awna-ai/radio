use radio_panels::{PanelResult, evaluate_fixture, render_markdown};
use serde_json::Value;
use std::{fs, path::Path};

fn assert_exact(actual: &Value, expected: &Value, path: &str) -> Result<(), String> {
    let mismatch = || format!("{path}: expected {expected}, got {actual}");
    match expected {
        Value::Object(expected) => {
            let actual = actual.as_object().ok_or_else(mismatch)?;
            if actual.len() != expected.len()
                || expected.keys().any(|key| !actual.contains_key(key))
            {
                return Err(format!("{path}: object keys differ"));
            }
            for (key, value) in expected {
                assert_exact(&actual[key], value, &format!("{path}.{key}"))?;
            }
        }
        Value::Array(expected) => {
            let actual = actual.as_array().ok_or_else(mismatch)?;
            if actual.len() != expected.len() {
                return Err(format!(
                    "{path}: expected {} elements, got {}",
                    expected.len(),
                    actual.len()
                ));
            }
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_exact(actual, expected, &format!("{path}[{index}]"))?;
            }
        }
        Value::Number(expected) => {
            let expected = expected.as_f64().ok_or_else(mismatch)?;
            let actual = actual.as_f64().ok_or_else(mismatch)?;
            if actual != expected {
                return Err(mismatch());
            }
        }
        _ if actual != expected => return Err(mismatch()),
        _ => {}
    }
    Ok(())
}

#[test]
fn all_compensated_sum_probes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference_probes.json");
    let probes: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("probe file")).expect("probe JSON");
    let entries = probes["rounds"].as_array().expect("rounds probes");
    assert_eq!(entries.len(), 12);
    let mut failures = Vec::new();
    for entry in entries {
        let name = entry["name"].as_str().expect("probe name");
        let mut errors = Vec::new();
        match evaluate_fixture(&entry["fixture"]) {
            Ok(actual) => {
                if let Err(error) = assert_exact(&actual["result"], &entry["result"], "result") {
                    errors.push(error);
                }
                let markdown = serde_json::from_value::<PanelResult>(actual["result"].clone())
                    .map_err(|error| error.to_string())
                    .and_then(|result| render_markdown(&result).map_err(|error| error.to_string()));
                match markdown {
                    Ok(actual) => {
                        let expected = entry["markdown"].as_str().expect("expected markdown");
                        if actual != expected {
                            errors.push(format!(
                                "markdown differs: expected {expected:?}, got {actual:?}"
                            ));
                        }
                    }
                    Err(error) => errors.push(format!("markdown: {error}")),
                }
            }
            Err(error) => errors.push(format!("evaluate_fixture: {error}")),
        }
        if !errors.is_empty() {
            failures.push(format!("{name}: {}", errors.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} summation probe failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
