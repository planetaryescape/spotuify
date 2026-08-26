//! Parity contract between the Rust `DaemonEvent` roster and clients that
//! must mirror it (notably the macOS `DaemonEvent` enum).
//!
//! The canonical fixture lives in the macOS test bundle so Swift can load it as
//! a resource. Each entry is a kind plus a minimal valid frame of that kind, so
//! the Swift side can prove its decoder really handles the kind rather than
//! asserting against a hand-kept list of names. Run with
//! `UPDATE_EVENT_KINDS_FIXTURE=1` to regenerate after adding an event variant.
//!
//! Swift falling back to `.unknown` keeps the app alive against a newer daemon;
//! it is not a licence to skip a case. The fixture lists every real kind so the
//! Swift side has to decode each one deliberately.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use serde_json::{json, Value};
use spotuify_protocol::DaemonEvent;

mod common;

use common::sample_frames;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../clients/macos/Tests/SpotuifyKitTests/Fixtures/event-kinds.json")
}

/// `[{ "kind": ..., "sample": <frame> }]`, sorted by kind.
fn fixture_entries() -> Vec<Value> {
    let mut entries: Vec<Value> = sample_frames()
        .into_iter()
        .map(|sample| {
            let kind = sample["event"].as_str().unwrap().to_string();
            json!({ "kind": kind, "sample": sample })
        })
        .collect();
    entries.sort_by(|left, right| left["kind"].as_str().cmp(&right["kind"].as_str()));
    entries
}

#[test]
fn all_kind_labels_is_sorted_unique_and_complete() {
    let labels = DaemonEvent::all_kind_labels();
    // 40 event kinds. `Unknown` is not one of them — it is the client-side
    // fallback for a tag this build doesn't have.
    assert_eq!(labels.len(), 40, "event kind count changed");

    let mut sorted = labels.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.as_slice(),
        labels,
        "all_kind_labels must be sorted and free of duplicates"
    );
}

#[test]
fn every_roster_kind_has_a_sample_frame() {
    // The roster is generated from the enum by `daemon_event_kinds!`, so the
    // compiler catches a missing kind there. Nothing catches a missing *sample*
    // except this: without it, a new kind would ship to the macOS fixture with
    // no probe frame and Swift would never be asked to decode it.
    let sampled: Vec<String> = fixture_entries()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sampled,
        DaemonEvent::all_kind_labels(),
        "sample_frames() and all_kind_labels() disagree; add the new event to \
         tests/common/mod.rs"
    );
}

#[test]
fn rust_roster_matches_macos_fixture() {
    let entries = fixture_entries();
    let serialized = serde_json::to_string_pretty(&entries).unwrap() + "\n";
    let path = fixture_path();

    if std::env::var_os("UPDATE_EVENT_KINDS_FIXTURE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &serialized).unwrap();
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "event-kinds fixture missing at {} ({err}); regenerate with \
             UPDATE_EVENT_KINDS_FIXTURE=1",
            path.display()
        )
    });
    let on_disk: Vec<Value> = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(
        on_disk, entries,
        "macOS event-kinds fixture is stale; regenerate with \
         UPDATE_EVENT_KINDS_FIXTURE=1 and add any missing DaemonEvent case"
    );
}
