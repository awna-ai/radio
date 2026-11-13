use serde_json::Value;

pub fn contained(recorded: &Value, actual: &Value, key: &str) -> bool {
    match (recorded, actual) {
        (Value::String(s), Value::Number(n)) if s == "<time>" => n
            .as_f64()
            .is_some_and(|time| time.is_finite() && (1e9..=1e11).contains(&time)),
        (Value::String(s), Value::String(_)) if s == "<string>" => true,
        (Value::Object(expected), Value::Object(got)) => expected.iter().all(|(key, value)| {
            got.get(key)
                .is_some_and(|actual| contained(value, actual, key))
        }),
        (Value::Array(expected), Value::Array(got)) if ["ops", "majors"].contains(&key) => {
            expected.iter().all(|value| got.contains(value))
        }
        (Value::Array(expected), Value::Array(got)) => {
            expected.len() == got.len()
                && expected
                    .iter()
                    .zip(got)
                    .all(|(value, actual)| contained(value, actual, ""))
        }
        _ => recorded == actual,
    }
}

#[test]
fn matching_is_additive_but_preserves_order_values_and_types() {
    use serde_json::json;
    assert!(contained(
        &json!({"a":"<time>","b":"<string>","ops":["hello"],"majors":[1]}),
        &json!({"a":1.7e9,"b":"ok","ops":["new","hello"],"majors":[2,1],"extra":true}),
        ""
    ));
    for (recorded, actual) in [
        (json!({"a":1}), json!({"b":1})),
        (json!([1, 2]), json!([2, 1])),
        (json!([1]), json!([1, 2])),
        (json!("<time>"), json!("1")),
        (json!("<time>"), json!(1.7e12)),
        (json!("<time>"), json!(0)),
        (json!("<time>"), json!(999999999)),
        (json!("<time>"), json!(100000000001_u64)),
        (json!("<string>"), json!(1)),
        (json!(true), json!(false)),
        (json!({"ops":["hello"]}), json!({"ops":[]})),
        (json!({"majors":[1]}), json!({"majors":[2]})),
    ] {
        assert!(!contained(&recorded, &actual, ""), "{recorded} {actual}");
    }
}
