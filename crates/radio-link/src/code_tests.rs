use super::*;
use data_encoding::BASE64URL_NOPAD;
use sha2::Digest;
fn code() -> JoinCode {
    JoinCode {
        fleet: "ab".repeat(32),
        inviter: iroh::SecretKey::generate().public(),
        secret: [255; 32],
        hints: Hints::default(),
    }
}
fn parts(code: &JoinCode) -> Vec<String> {
    vec![
        PREFIX.into(),
        code.fleet.clone(),
        code.inviter.to_string(),
        BASE64URL_NOPAD.encode(&code.secret),
        BASE64URL_NOPAD.encode(&serde_json::to_vec(&code.hints).unwrap()),
    ]
}
fn refused(parts: &[String]) {
    assert_eq!(
        JoinCode::parse(&parts.join(".")).err(),
        Some("not a join code")
    );
}
#[test]
fn round_trips_with_all_hint_counts_and_new_secrets() {
    for count in [0, 1, 8] {
        let direct = (0..count)
            .map(|n| {
                if n % 2 == 0 {
                    format!("192.0.2.{}:9000", n + 1)
                } else {
                    format!("[2001:db8::{}]:9000", n + 1)
                }
                .parse()
                .unwrap()
            })
            .collect();
        let original = JoinCode::new("ab".repeat(32), code().inviter, Hints { direct }).unwrap();
        assert_eq!(JoinCode::parse(&original.encode()).unwrap(), original);
        assert_ne!(
            original.secret,
            JoinCode::new(
                original.fleet.clone(),
                original.inviter,
                original.hints.clone()
            )
            .unwrap()
            .secret
        );
    }
}
#[test]
fn hash_and_debug_keep_secret_private() {
    let code = code();
    assert_eq!(
        code.hash().as_slice(),
        sha2::Sha256::digest(code.secret).as_slice()
    );
    let debug = format!("{code:?}");
    assert!(debug.contains("<hidden>"));
    assert!(!debug.contains(&BASE64URL_NOPAD.encode(&code.secret)));
    assert!(!debug.contains(&data_encoding::HEXLOWER.encode(&code.secret)));
}
#[test]
fn refuses_malformed_code_and_identity() {
    let c = code();
    let valid = parts(&c);
    for text in [
        "".into(),
        format!(" {}", c.encode()),
        format!("{}\n", c.encode()),
        "a".repeat(CODE_MAX_CHARS + 1),
    ] {
        assert!(JoinCode::parse(&text).is_err());
    }
    let mut p = valid.clone();
    p.remove(0);
    refused(&p);
    let mut p = valid.clone();
    p.push("extra".into());
    refused(&p);
    for prefix in ["rj2", ""] {
        let mut p = valid.clone();
        p[0] = prefix.into();
        refused(&p);
    }
    for fleet in ["AB".repeat(32), "a".repeat(63)] {
        let mut p = valid.clone();
        p[1] = fleet;
        refused(&p);
    }
    let invalid = (0..=255)
        .map(|n| [n; 32])
        .find(|b| iroh::PublicKey::from_bytes(b).is_err())
        .unwrap();
    let mut p = valid;
    p[2] = data_encoding::HEXLOWER.encode(&invalid);
    refused(&p);
}
#[test]
fn refuses_secret_lengths_alphabet_padding_and_trailing_bits() {
    let valid = parts(&code());
    for secret in [
        BASE64URL_NOPAD.encode(&[7; 31]),
        BASE64URL_NOPAD.encode(&[7; 33]),
        format!("{}=", valid[3]),
        "+".repeat(43),
        "/".repeat(43),
    ] {
        let mut p = valid.clone();
        p[3] = secret;
        refused(&p);
    }
    let mut p = valid;
    p[3].pop();
    p[3].push('9');
    refused(&p);
}
#[test]
fn refuses_unknown_oversized_and_unusable_hints() {
    let valid = parts(&code());
    for json in [
        "{\"direct\":[],\"extra\":true}".to_owned(),
        "not json".into(),
        serde_json::to_string(&Hints {
            direct: vec!["192.0.2.1:1".parse().unwrap(); 9],
        })
        .unwrap(),
    ] {
        let mut p = valid.clone();
        p[4] = BASE64URL_NOPAD.encode(json.as_bytes());
        refused(&p);
    }
    for addr in [
        "0.0.0.0:1",
        "[::]:1",
        "224.0.0.1:1",
        "[ff02::1]:1",
        "255.255.255.255:1",
        "[::ffff:255.255.255.255]:1",
    ] {
        let mut p = valid.clone();
        p[4] = BASE64URL_NOPAD.encode(
            &serde_json::to_vec(&Hints {
                direct: vec![addr.parse().unwrap()],
            })
            .unwrap(),
        );
        refused(&p);
    }
}

#[test]
fn hints_json_size_cap_counts_whitespace() {
    let valid = parts(&code());
    for size in [1024, 1025] {
        let json = format!("{:<size$}", r#"{"direct":[]}"#);
        assert_eq!(json.len(), size);
        let mut p = valid.clone();
        p[4] = BASE64URL_NOPAD.encode(json.as_bytes());
        if size == 1024 {
            assert_eq!(
                JoinCode::parse(&p.join(".")).unwrap().hints,
                Hints::default()
            );
        } else {
            refused(&p);
        }
    }
}

#[test]
fn refuses_hints_with_port_zero() {
    let c = code();
    for addr in ["192.0.2.1:0", "[2001:db8::1]:0"] {
        let hints = Hints {
            direct: vec![addr.parse().unwrap()],
        };
        let mut p = parts(&c);
        p[4] = BASE64URL_NOPAD.encode(&serde_json::to_vec(&hints).unwrap());
        refused(&p);
        assert_eq!(
            JoinCode::new(c.fleet.clone(), c.inviter, hints).err(),
            Some("the join code could not be made")
        );
    }
}

#[test]
fn refuses_hints_with_repeated_addresses() {
    let c = code();
    for addr in ["192.0.2.1:9000", "[2001:db8::1]:9000"] {
        let hints = Hints {
            direct: vec![addr.parse().unwrap(); 2],
        };
        let mut p = parts(&c);
        p[4] = BASE64URL_NOPAD.encode(&serde_json::to_vec(&hints).unwrap());
        refused(&p);
        assert_eq!(
            JoinCode::new(c.fleet.clone(), c.inviter, hints).err(),
            Some("the join code could not be made")
        );
    }
}
