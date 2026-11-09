// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! Public YAML/SDK compatibility witnesses for the locally corrected backend.
use serde_yaml_ng::{Deserializer, Value};

#[test]
fn yaml_value_is_the_sdks_public_value_type() {
    let doc: Value = serde_yaml_ng::from_str("models:\n  one: { provider: mock }\n").unwrap();
    let facts = busbar_contract::plane_calls::dialect_facts(&doc, |name| {
        Some((format!("{name}-protocol"), Default::default()))
    });
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].protocol, "mock-protocol");
}

#[test]
fn sdk_section_split_preserves_hooks_and_entry_order() {
    let split = busbar_contract::section::split_section::<_, Value>(
        Deserializer::from_str("hooks: [compress]\nfirst: {model: one}\nsecond: {model: two}\n"),
        "pools",
        "pool",
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(split.hooks, ["compress"]);
    assert_eq!(
        split.entries.keys().map(String::as_str).collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn aliases_and_explicit_merge_preserve_override() {
    let mut doc: Value =
        serde_yaml_ng::from_str("base: &base {a: 1, b: true}\nitem: {<<: *base, a: 2}\n").unwrap();
    doc.apply_merge().unwrap();
    assert_eq!(doc["item"]["a"].as_u64(), Some(2));
    assert_eq!(doc["item"]["b"].as_bool(), Some(true));
}

#[test]
fn duplicate_mapping_keys_are_refused() {
    assert!(serde_yaml_ng::from_str::<Value>("a: 1\na: 2\n").is_err());
}

#[test]
fn malformed_yaml_is_refused() {
    assert!(serde_yaml_ng::from_str::<Value>("[broken").is_err());
}

#[test]
fn tagged_values_remain_tagged() {
    let tagged: Value = serde_yaml_ng::from_str("!variant payload").unwrap();
    assert!(matches!(tagged, Value::Tagged(_)));
}

#[test]
fn owned_reader_data_outlives_the_parser() {
    let doc: Value = serde_yaml_ng::from_reader(std::io::Cursor::new(b"word: text\n")).unwrap();
    assert_eq!(doc["word"].as_str(), Some("text"));
}

#[test]
fn serialization_roundtrip_preserves_values() {
    let doc: Value = serde_yaml_ng::from_str("list: [1, true, null, text]\n").unwrap();
    let serialized = serde_yaml_ng::to_string(&doc).unwrap();
    assert_eq!(serde_yaml_ng::from_str::<Value>(&serialized).unwrap(), doc);
}
