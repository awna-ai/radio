mod common;

use common::assert_vector;
use radio_panels::{
    Kind, PanelResult, clean, fence, leak_findings, match_key, parse_block, render_markdown,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

#[test]
fn all_reference_probes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference_probes.json");
    let probes: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("probe file")).expect("probe JSON");
    let mut failures = Vec::new();
    for probe in probes["parse"].as_array().expect("parse probes") {
        let name = probe["name"].as_str().expect("probe name");
        let input = &probe["input"];
        let kind = match input["kind"].as_str().expect("kind") {
            "list" => Some(Kind::List),
            "text" => Some(Kind::Text),
            _ => None,
        };
        let check = || -> Result<(), String> {
            let actual = if let Some(kind) = kind {
                let crew: Vec<String> =
                    serde_json::from_value(input["crew"].clone()).map_err(|e| e.to_string())?;
                let known: BTreeSet<String> = serde_json::from_value(input["known_drafts"].clone())
                    .map_err(|e| e.to_string())?;
                let block = parse_block(
                    input["post"].as_str().expect("post"),
                    kind,
                    input["n"].as_i64(),
                    input["round"]
                        .as_u64()
                        .expect("round")
                        .try_into()
                        .expect("u32 round"),
                    input["author"].as_str().expect("author"),
                    &crew,
                    &known,
                );
                match block {
                    Some(block) => json!({"block": block}),
                    None => json!({"malformed": true}),
                }
            } else {
                // An unknown kind must have a malformed oracle, even though Rust cannot represent it.
                if input["kind"] != "list"
                    && input["kind"] != "text"
                    && probe["expect"] != json!({"malformed": true})
                {
                    return Err("unknown kind must be expected malformed".into());
                }
                json!({"malformed": true})
            };
            assert_vector(&actual, &probe["expect"], name, false)
        };
        if let Err(error) = check() {
            failures.push(format!("{name} ({}): {error}", path.display()));
        }
    }
    for (category, entries) in probes["text"].as_object().expect("text probes") {
        for (index, entry) in entries.as_array().expect("text list").iter().enumerate() {
            let name = format!("text.{category}[{index}]");
            let input = entry[0].as_str().expect("text input");
            let actual: Result<Value, String> = match category.as_str() {
                "clean" => Ok(json!(clean(
                    input,
                    entry[1]
                        .as_u64()
                        .expect("limit")
                        .try_into()
                        .expect("usize limit")
                ))),
                "match_key" => Ok(json!(match_key(input))),
                "fence" => fence(input, entry[1].as_str().expect("fence text"))
                    .map(|v| json!(v))
                    .map_err(|e| e.to_string()),
                "leaks" => {
                    let terms: Vec<String> =
                        serde_json::from_value(entry[1].clone()).expect("terms");
                    Ok(json!(leak_findings(input, &terms)))
                }
                _ => Err(format!("unknown text category {category}")),
            };
            let expected = entry
                .as_array()
                .expect("text tuple")
                .last()
                .expect("expected value");
            // Text probe outputs are exact, including every string and category.
            let check = actual.and_then(|actual| {
                if actual == *expected {
                    Ok(())
                } else {
                    Err(format!("expected {expected}, got {actual}"))
                }
            });
            if let Err(error) = check {
                failures.push(format!("{name} ({}): {error}", path.display()));
            }
        }
    }
    for (index, probe) in probes["markdown"]
        .as_array()
        .expect("markdown probes")
        .iter()
        .enumerate()
    {
        let check = || -> Result<(), String> {
            let result: PanelResult =
                serde_json::from_value(probe["result"].clone()).map_err(|e| e.to_string())?;
            let actual = render_markdown(&result).map_err(|e| e.to_string())?;
            if actual == probe["markdown"].as_str().ok_or("expected markdown")? {
                Ok(())
            } else {
                Err(format!("markdown differs: {actual:?}"))
            }
        };
        if let Err(error) = check() {
            failures.push(format!("markdown[{index}] ({}): {error}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} probe failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
