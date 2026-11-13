use std::fs;

fn contract_number(label: &str) -> u32 {
    let contract = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CONTRACT.md"))
        .expect("CONTRACT.md must be readable");
    let prefix = format!("{label}: ");
    let values: Vec<_> = contract
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect();
    assert_eq!(values.len(), 1, "expected exactly one {label}: line");
    values[0]
        .parse()
        .unwrap_or_else(|_| panic!("{label} must be a u32"))
}

#[test]
fn contract_major_matches_the_crate() {
    assert_eq!(
        contract_number("Protocol major"),
        radio_client::PROTOCOL_MAJOR
    );
}

#[test]
fn contract_minor_matches_the_crate() {
    assert_eq!(
        contract_number("Protocol minor"),
        radio_client::PROTOCOL_MINOR
    );
}

#[test]
fn file_ops_and_minor_seven_are_available() {
    for op in [
        "send",
        "delivery_folder_set",
        "delivery_folder_clear",
        "deliveries",
    ] {
        assert!(radio_client::OPS.contains(&op), "{op}");
    }
    assert_eq!(radio_client::PROTOCOL_MINOR, 7);
}

#[test]
fn file_requests_round_trip_and_refuse_unknown_fields() {
    use radio_client::Request;
    use serde_json::json;
    for value in [
        json!({"op":"send","app":"notes","computer":"garden","name":"letter","file":"/letter"}),
        json!({"op":"delivery_folder_set","app":"notes","folder":"/inbox"}),
        json!({"op":"delivery_folder_clear","app":"notes"}),
        json!({"op":"deliveries","app":"notes"}),
        json!({"op":"deliveries","app":"notes","after":0,"limit":100}),
        json!({"op":"deliveries","app":"notes","after":u64::MAX,"limit":500}),
    ] {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        let mut unknown = value;
        unknown["extra"] = true.into();
        assert!(serde_json::from_value::<Request>(unknown).is_err());
    }
    for value in [json!(-1), json!(1.5), json!("1")] {
        assert!(
            serde_json::from_value::<Request>(
                json!({"op":"deliveries","app":"notes","after":value})
            )
            .is_err()
        );
    }
}

#[test]
fn delivery_answers_round_trip() {
    let value = serde_json::json!({"ok":true,"size":5,"deliveries":[{"id":7,"computer":"birch","name":"letter","size":5,"at":1700000000.5}]});
    let response: radio_client::Response = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), value);
}

#[test]
fn delivery_type_round_trips_and_accepts_future_fields() {
    let value =
        serde_json::json!({"id":7,"computer":"birch","name":"letter","size":5,"at":1700000000.5});
    let delivery: radio_client::Delivery = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&delivery).unwrap(), value);
    let mut future = value;
    future["later"] = true.into();
    assert_eq!(
        serde_json::from_value::<radio_client::Delivery>(future).unwrap(),
        delivery
    );
}
