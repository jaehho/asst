//! The `--json` other programs read, kept as samples in `contract/` at the
//! top of the repo, with `consumers.json` saying who calls what and which
//! fields they read (DESIGN.md, "Integrations").
//!
//! The CLI prints the daemon's reply as it is, and the daemon serializes
//! these types, so a sample that survives a round trip through its type is
//! exactly what `asst --json` prints for that data. A consumer's fake
//! answers from these files; changing one is changing what it is promised.

use std::path::PathBuf;

use asst_core::api::{Added, ListView, TaskView};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contract")
}

fn load(name: &str) -> Value {
    let path = dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn again<T: Serialize + DeserializeOwned>(name: &str, sample: &Value) -> Value {
    let parsed: T = serde_json::from_value(sample.clone())
        .unwrap_or_else(|e| panic!("{name} is not what asst prints: {e}"));
    serde_json::to_value(parsed).unwrap()
}

/// Which command prints each sample. A new sample needs its line here.
fn round_trip(name: &str, sample: &Value) -> Value {
    match name {
        "add.json" => again::<Added>(name, sample),
        "show.json" => again::<TaskView>(name, sample),
        "ls.json" | "completed.json" => again::<Vec<TaskView>>(name, sample),
        "lists.json" => again::<Vec<ListView>>(name, sample),
        _ => panic!("{name}: no type for this sample; add it to round_trip"),
    }
}

fn samples() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .expect("contract/ is there")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json") && n != "consumers.json")
        .collect();
    names.sort();
    names
}

#[test]
fn every_sample_is_what_asst_prints() {
    let names = samples();
    assert!(names.len() >= 5, "the samples were found: {names:?}");
    for name in names {
        let sample = load(&name);
        assert_eq!(
            round_trip(&name, &sample),
            sample,
            "{name} came back different: a field was added, renamed or dropped"
        );
    }
}

#[test]
fn every_consumer_finds_the_fields_it_reads() {
    let consumers = load("consumers.json");
    let consumers = consumers.as_object().expect("an object of consumers");
    assert!(!consumers.is_empty());
    for (who, c) in consumers {
        let reads = c["reads"].as_object().expect("reads: sample -> fields");
        for (name, fields) in reads {
            let rows = match load(name) {
                Value::Array(rows) => rows,
                one => vec![one],
            };
            assert!(!rows.is_empty(), "{name} needs a row for {who} to read");
            for row in &rows {
                for field in fields.as_array().expect("a list of fields") {
                    let field = field.as_str().unwrap();
                    assert!(
                        row.get(field).is_some_and(|v| !v.is_null()),
                        "{who} reads `{field}` from {name} ({}); \
                         move {who} off it before dropping it",
                        c["code"].as_str().unwrap_or("?")
                    );
                }
            }
        }
    }
}
