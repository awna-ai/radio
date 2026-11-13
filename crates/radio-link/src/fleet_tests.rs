use super::*;

fn keys() -> (iroh::SecretKey, iroh::SecretKey) {
    (iroh::SecretKey::generate(), iroh::SecretKey::generate())
}
fn body(node: &iroh::SecretKey, recovery: &iroh::SecretKey) -> FleetBody {
    FleetBody {
        id: "ab".repeat(32),
        name: "lantern".into(),
        version: 1,
        epoch: 0,
        founder: node.public().to_string(),
        recovery: recovery.public().to_string(),
        release_key: None,
        relays: vec![],
    }
}
fn invalid_point() -> String {
    (0..=255)
        .map(|n| [n; 32])
        .find(|b| iroh::PublicKey::from_bytes(b).is_err())
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[test]
fn founding_record_has_fresh_id_and_distinct_recovery() {
    let node = iroh::SecretKey::generate();
    let (record, recovery) = found(&node, "juniper").unwrap();
    assert_eq!(record.verify(), Ok(()));
    assert_eq!(record.body.founder, node.public().to_string());
    assert_eq!(record.body.recovery, recovery.public().to_string());
    assert_ne!(record.body.founder, record.body.recovery);
    assert_eq!(record.body.version, 1);
    assert_eq!(record.body.epoch, 0);
    assert!(record.body.relays.is_empty());
    assert_eq!(record.body.release_key, None);
    assert!(crate::wire::hex64(&record.body.id));
    assert_ne!(record.body.id, found(&node, "juniper").unwrap().0.body.id);
    assert_eq!(found(&node, "Juniper").err(), Some("not a fleet name"));
    assert_eq!(accept(None, &record), Ok(()));
}
#[test]
fn signature_is_bound_to_body_signer_and_domain() {
    let (node, recovery) = keys();
    let b = body(&node, &recovery);
    let mut record = sign(b.clone(), Signer::Founder, &node);
    record.body.name = "garden".into();
    assert_eq!(
        record.verify(),
        Err("the fleet record's signature does not verify")
    );
    assert!(
        sign(b.clone(), Signer::Founder, &recovery)
            .verify()
            .is_err()
    );
    assert!(sign(b.clone(), Signer::Recovery, &node).verify().is_err());
    assert!(signed_bytes(&b).starts_with(RECORD_DOMAIN));
    let mut record = sign(b.clone(), Signer::Founder, &node);
    record.signature = node
        .sign(&serde_json::to_vec(&b).unwrap())
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(record.verify().is_err());
}
#[test]
fn acceptance_checks_identity_version_and_authority() {
    let (node, recovery) = keys();
    let b = body(&node, &recovery);
    let current = sign(b.clone(), Signer::Founder, &node);
    for version in [0, 1] {
        let mut next = b.clone();
        next.version = version;
        assert!(accept(Some(&current), &sign(next, Signer::Founder, &node)).is_err());
    }
    let mut newer = b.clone();
    newer.version = 3;
    let newer = sign(newer, Signer::Founder, &node);
    for version in [2, 3] {
        let mut next = b.clone();
        next.version = version;
        assert_eq!(
            accept(Some(&newer), &sign(next, Signer::Founder, &node)),
            Err("the fleet record is not newer")
        );
    }
    let mut next = b.clone();
    next.version = 2;
    next.id = "cd".repeat(32);
    assert_eq!(
        accept(Some(&current), &sign(next, Signer::Founder, &node)),
        Err("the fleet record is for another fleet")
    );
    let replacement = iroh::SecretKey::generate();
    let mut next = b.clone();
    next.version = 2;
    next.name = "garden".into();
    next.release_key = Some(replacement.public().to_string());
    assert_eq!(
        accept(Some(&current), &sign(next, Signer::Founder, &node)),
        Ok(())
    );
    let mut next = b.clone();
    next.version = 2;
    next.founder = replacement.public().to_string();
    assert_eq!(
        accept(Some(&current), &sign(next.clone(), Signer::Founder, &node)),
        Err("the fleet record's signature does not verify")
    );
    assert_eq!(
        accept(
            Some(&current),
            &sign(next.clone(), Signer::Founder, &replacement)
        ),
        Err("only the recovery key names a new founding member")
    );
    next.epoch = 1;
    next.release_key = Some(iroh::SecretKey::generate().public().to_string());
    let replaced = sign(next.clone(), Signer::Recovery, &recovery);
    assert_eq!(accept(Some(&current), &replaced), Ok(()));
    next.version = 3;
    assert_eq!(
        accept(Some(&replaced), &sign(next, Signer::Founder, &replacement)),
        Ok(())
    );
    let mut next = b.clone();
    next.version = 2;
    next.epoch = 1;
    next.name = "orchard".into();
    assert_eq!(
        accept(Some(&current), &sign(next, Signer::Recovery, &recovery)),
        Err("the recovery key changes only the founding member and the release key")
    );
    let mut next = b;
    next.version = 2;
    next.recovery = replacement.public().to_string();
    assert_eq!(
        accept(Some(&current), &sign(next, Signer::Recovery, &replacement)),
        Err("the fleet record changes its recovery key")
    );
}
#[test]
fn invalid_fields_and_unknown_fields_are_refused() {
    let (node, recovery) = keys();
    let b = body(&node, &recovery);
    let mut invalid = vec![];
    let mut x = b.clone();
    x.relays.push("garden".into());
    invalid.push(x);
    let mut x = b.clone();
    x.id = x.id.to_uppercase();
    invalid.push(x);
    let mut x = b.clone();
    x.release_key = Some(invalid_point());
    invalid.push(x);
    let mut x = b.clone();
    x.recovery = x.founder.clone();
    invalid.push(x);
    let mut x = b.clone();
    x.version = 0;
    invalid.push(x);
    let mut x = b.clone();
    x.founder = x.founder.to_uppercase();
    invalid.push(x);
    let mut x = b.clone();
    x.recovery = x.recovery.to_uppercase();
    invalid.push(x);
    let mut x = b.clone();
    x.release_key = Some(node.public().to_string().to_uppercase());
    invalid.push(x);

    let mut x = b.clone();
    x.name = "Garden".into();
    invalid.push(x);
    for x in invalid {
        assert_eq!(
            sign(x, Signer::Founder, &node).verify(),
            Err("not a fleet record")
        );
    }
    for signature in ["f".repeat(127), "F".repeat(128), "z".repeat(128)] {
        let mut record = sign(b.clone(), Signer::Founder, &node);
        record.signature = signature;
        assert_eq!(record.verify(), Err("not a fleet record"));
    }
    let mut json = serde_json::to_value(sign(b, Signer::Founder, &node)).unwrap();
    json["extra"] = true.into();
    assert!(serde_json::from_value::<FleetRecord>(json.clone()).is_err());
    json.as_object_mut().unwrap().remove("extra");
    json["body"]["extra"] = true.into();
    assert!(serde_json::from_value::<FleetRecord>(json).is_err());
}

#[test]
fn recovery_starts_new_epoch_after_maximum_founder_version() {
    let (node, recovery) = keys();
    let b = body(&node, &recovery);
    let current = sign(b.clone(), Signer::Founder, &node);
    let mut next = b;
    next.version = u64::MAX;
    let poisoned = sign(next.clone(), Signer::Founder, &node);
    assert_eq!(accept(Some(&current), &poisoned), Ok(()));

    let replacement = iroh::SecretKey::generate();
    next.epoch += 1;
    next.version = 1;
    next.founder = replacement.public().to_string();
    let recovered = sign(next.clone(), Signer::Recovery, &recovery);
    assert_eq!(accept(Some(&poisoned), &recovered), Ok(()));

    next.version = 2;
    assert_eq!(
        accept(Some(&recovered), &sign(next, Signer::Founder, &replacement)),
        Ok(())
    );
    assert_eq!(
        accept(Some(&recovered), &poisoned),
        Err("only the recovery key starts a new epoch")
    );
}

#[test]
fn founder_cannot_start_another_epoch() {
    let (node, recovery) = keys();
    let mut next = body(&node, &recovery);
    let current = sign(next.clone(), Signer::Founder, &node);
    next.version = 2;
    next.epoch = 1;
    assert_eq!(
        accept(Some(&current), &sign(next, Signer::Founder, &node)),
        Err("only the recovery key starts a new epoch")
    );
}

#[test]
fn recovery_cannot_reuse_or_lower_an_epoch_even_with_a_newer_version() {
    let (node, recovery) = keys();
    let mut next = body(&node, &recovery);
    next.epoch = 1;
    let current = sign(next.clone(), Signer::Recovery, &recovery);
    for epoch in [0, 1] {
        next.epoch = epoch;
        next.version = 2;
        assert_eq!(
            accept(
                Some(&current),
                &sign(next.clone(), Signer::Recovery, &recovery)
            ),
            Err("the fleet record is not newer")
        );
    }
}
