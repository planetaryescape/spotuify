//! Parity contract between the Rust `DaemonEvent` roster and clients that
//! must mirror it (notably the macOS `DaemonEvent` enum).
//!
//! The canonical fixture lives in the macOS test bundle so Swift can load it as
//! a resource; this test asserts the Rust side (`DaemonEvent::all_kind_labels`)
//! stays equal to that fixture. Run with `UPDATE_EVENT_KINDS_FIXTURE=1` to
//! regenerate it after adding an event variant.
//!
//! Swift falling back to `.unknown` keeps the app alive against a newer daemon;
//! it is not a licence to skip a case. The fixture lists every real kind so the
//! Swift side has to decode each one deliberately.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use spotuify_protocol::DaemonEvent;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../clients/macos/Tests/SpotuifyKitTests/Fixtures/event-kinds.json")
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
fn rust_roster_matches_macos_fixture() {
    let labels = DaemonEvent::all_kind_labels();
    let serialized = serde_json::to_string_pretty(labels).unwrap() + "\n";
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
    let on_disk: Vec<String> = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(
        on_disk, labels,
        "macOS event-kinds fixture is stale; regenerate with \
         UPDATE_EVENT_KINDS_FIXTURE=1 and add any missing DaemonEvent case"
    );
}
