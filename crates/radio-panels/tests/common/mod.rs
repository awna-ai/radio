use serde_json::Value;

pub fn assert_vector(
    actual: &Value,
    expected: &Value,
    path: &str,
    partial: bool,
) -> Result<(), String> {
    let mismatch = || format!("{path}: expected {expected}, got {actual}");
    match expected {
        Value::Object(expected) => {
            let actual = actual.as_object().ok_or_else(mismatch)?;
            if !partial
                && actual.keys().collect::<std::collections::BTreeSet<_>>()
                    != expected.keys().collect()
            {
                return Err(format!("{path}: object keys differ"));
            }
            for (key, value) in expected {
                let next = format!("{path}.{key}");
                let actual = actual
                    .get(key)
                    .ok_or_else(|| format!("{next}: missing key"))?;
                assert_vector(actual, value, &next, partial)?;
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
                assert_vector(actual, expected, &format!("{path}[{index}]"), partial)?;
            }
        }
        Value::Number(expected_number) => {
            let expected_float = expected_number.as_f64().ok_or_else(mismatch)?;
            let actual_float = actual.as_f64().ok_or_else(mismatch)?;
            let equal = if expected_number.is_f64() {
                (actual_float - expected_float).abs() <= 1e-9
            } else {
                actual_float == expected_float
            };
            if !equal {
                return Err(mismatch());
            }
        }
        _ if actual != expected => return Err(mismatch()),
        _ => {}
    }
    Ok(())
}
