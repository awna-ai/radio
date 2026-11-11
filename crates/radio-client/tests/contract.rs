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
