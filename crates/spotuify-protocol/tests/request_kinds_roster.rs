//! Parity contract between the Rust `Request` roster and clients that
//! must mirror it (notably the macOS `DaemonRequest` enum).
//!
//! The canonical fixture lives in the macOS test bundle so Swift can load
//! it as a resource; this test asserts the Rust side (`Request::all_kind_labels`)
//! stays equal to that fixture. Run with `UPDATE_REQUEST_KINDS_FIXTURE=1`
//! to regenerate it after adding a request variant.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use spotuify_protocol::Request;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../clients/macos/Tests/SpotuifyKitTests/Fixtures/request-kinds.json")
}

#[test]
fn all_kind_labels_is_sorted_unique_and_complete() {
    let labels = Request::all_kind_labels();
    // 83 distinct request kinds after provider discovery, target resolution,
    // daemon-owned audio-output enumeration, and wire-safe playlist previews.
    assert_eq!(labels.len(), 83, "request kind count changed");

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
    let labels = Request::all_kind_labels();
    let serialized = serde_json::to_string_pretty(labels).unwrap() + "\n";
    let path = fixture_path();

    if std::env::var_os("UPDATE_REQUEST_KINDS_FIXTURE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &serialized).unwrap();
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "request-kinds fixture missing at {} ({err}); regenerate with \
             UPDATE_REQUEST_KINDS_FIXTURE=1",
            path.display()
        )
    });
    let on_disk: Vec<String> = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(
        on_disk, labels,
        "macOS request-kinds fixture is stale; regenerate with \
         UPDATE_REQUEST_KINDS_FIXTURE=1 and add any missing DaemonRequest case"
    );
}
