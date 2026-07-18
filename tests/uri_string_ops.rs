#![allow(clippy::panic)]

//! Guard the provider-neutral URI boundary.
//!
//! Resource parsing and construction belong in `spotuify-core::ResourceUri`.
//! The only intentional string-level exceptions are Spotify's legacy URL/URI
//! normalizer, where Spotify-specific shapes are the external protocol being
//! translated.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", dir.display());
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to read directory entry: {err}"));
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

fn is_allowed(rel_path: &str, line: &str) -> bool {
    let line = line.trim();
    match rel_path {
        "crates/spotuify-spotify/src/selection.rs" => matches!(
            line,
            "let mut parts = trimmed.split(':');"
                | "let uri = format!(\"spotify:{kind}:{id}\");"
                | "let uri = format!(\"spotify:{}:{id}\", kind.to_ascii_lowercase());"
        ),
        _ => false,
    }
}

#[test]
fn non_core_code_uses_resource_uri_instead_of_spotify_string_operations() {
    let root = repo_root();
    let mut files = Vec::new();
    for source_root in ["crates", "src", "tests"] {
        rust_sources(&root.join(source_root), &mut files);
    }

    let forbidden = [
        ("URI segment splitting", concat!(".split", "(':')")),
        ("URI tail extraction", concat!(".rsplit", "(':')")),
        (
            "Spotify URI prefix matching",
            concat!(".starts_with(\"", "spotify:"),
        ),
        (
            "Spotify URI prefix stripping",
            concat!(".strip_prefix(\"", "spotify:"),
        ),
        (
            "Spotify URI prefix trimming",
            concat!(".trim_start_matches(\"", "spotify:"),
        ),
        (
            "Spotify URI string construction",
            concat!("format!(\"", "spotify:"),
        ),
    ];
    let mut violations = Vec::new();

    for path in files {
        let rel_path = path
            .strip_prefix(&root)
            .expect("source must be inside repository")
            .to_string_lossy()
            .replace('\\', "/");
        if rel_path == "tests/uri_string_ops.rs" || rel_path.starts_with("crates/spotuify-core/") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        for (index, line) in source.lines().enumerate() {
            for (label, pattern) in forbidden {
                if line.contains(pattern) && !is_allowed(&rel_path, line) {
                    violations.push(format!(
                        "{rel_path}:{}: {label}: {}",
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "use spotuify_core::ResourceUri; URI string operations require a documented, exact allowlist entry:\n{}",
        violations.join("\n")
    );
}
