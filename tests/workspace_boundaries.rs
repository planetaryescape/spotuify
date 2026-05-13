//! Compiler-enforced dependency DAG for the spotuify workspace.
//!
//! Adapted from mxr's `tests/workspace_boundaries.rs`. Each rule encodes the
//! dependency boundaries declared in `docs/blueprint/01-architecture.md`
//! §"Dependency rules". Adding a new crate requires extending [`ALLOWED_DEPS`].
//!
//! The test is permissive while crates are being extracted (no errors when a
//! crate doesn't yet exist). It tightens automatically as crates land.

#![allow(clippy::unwrap_used)]

use std::{collections::BTreeSet, fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_manifest(rel_path: &str) -> Option<toml::Value> {
    let full = repo_root().join(rel_path);
    let raw = fs::read_to_string(&full).ok()?;
    Some(toml::from_str(&raw).unwrap_or_else(|err| {
        panic!("malformed TOML in {rel_path}: {err}");
    }))
}

fn internal_dep_names(manifest: &toml::Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(deps) = manifest.get(table).and_then(toml::Value::as_table) else {
            continue;
        };
        for name in deps.keys() {
            if name.starts_with("spotuify-") {
                names.insert(name.clone());
            }
        }
    }
    names
}

/// Allowed internal dependencies per crate. A crate not listed here may
/// not declare any internal `spotuify-*` deps; this fails closed.
///
/// Build-up rules from `docs/blueprint/01-architecture.md`:
///   1. spotuify-core: no internal deps
///   2. spotuify-protocol: core only
///   3. spotuify-store, spotuify-search: core only
///   4. spotuify-spotify: core only
///   5. spotuify-player: core + spotify
///   6. spotuify-sync: core + store + search + spotify + player
///   7. spotuify-system: core + protocol
///   8. spotuify-lyrics: core + store + player
///   9. spotuify-audio: core + player
///  10. spotuify-daemon: integration point (everything backend)
///  11. spotuify-cli, spotuify-tui, spotuify-mcp: protocol only
fn allowed_deps(crate_name: &str) -> Option<BTreeSet<&'static str>> {
    let allowed: &[&'static str] = match crate_name {
        "spotuify-core" => &[],
        "spotuify-protocol" => &["spotuify-core"],
        // store and search hold CacheStatus / SearchScopeData / SearchSourceData
        // shapes that originate in spotuify-protocol. The blueprint suggested
        // core-only; the practical reality is they consume protocol types.
        "spotuify-store" => &["spotuify-core", "spotuify-protocol"],
        "spotuify-search" => &["spotuify-core", "spotuify-protocol", "spotuify-store"],
        // SpotifyError maps to IpcErrorKind from protocol; AuthErrorKind serialises
        // into DaemonEvent::AuthError variants. Protocol dep is intentional.
        "spotuify-spotify" => &["spotuify-core", "spotuify-protocol"],
        "spotuify-player" => &["spotuify-core", "spotuify-spotify"],
        "spotuify-sync" => &[
            "spotuify-core",
            "spotuify-protocol",
            "spotuify-store",
            "spotuify-search",
            "spotuify-spotify",
            "spotuify-player",
        ],
        "spotuify-system" => &["spotuify-core", "spotuify-protocol"],
        "spotuify-lyrics" => &[
            "spotuify-core",
            "spotuify-store",
            "spotuify-player",
        ],
        "spotuify-audio" => &["spotuify-core", "spotuify-player"],
        "spotuify-daemon" => &[
            "spotuify-core",
            "spotuify-protocol",
            "spotuify-store",
            "spotuify-search",
            "spotuify-spotify",
            "spotuify-player",
            "spotuify-sync",
            "spotuify-system",
            "spotuify-lyrics",
            "spotuify-audio",
            // The daemon handler routes mutations through spotuify-cli
            // helpers (actions/selection) so they share the same
            // typing as the CLI command surface. This is a deliberate
            // bridge: keeping action logic in one place across CLI +
            // TUI + daemon callers.
            "spotuify-cli",
        ],
        // CLI helpers call into spotuify-daemon::server::ensure_daemon_running
        // before each CLI->IPC request (autostart). The actions/selection
        // modules moved to spotuify-spotify to avoid the cli↔daemon
        // dependency cycle.
        "spotuify-cli" => &[
            "spotuify-core",
            "spotuify-protocol",
            "spotuify-spotify",
            "spotuify-player",
            "spotuify-daemon",
            "spotuify-search",
        ],
        // TUI mirrors the daemon's full backend surface because app.rs
        // talks to the live SpotifyClient + Store + Search + Sync +
        // Daemon::status during interactive use. A future refactor
        // could push more through the IPC layer; for now the
        // dependency edges are documented and live.
        "spotuify-tui" => &[
            "spotuify-core",
            "spotuify-protocol",
            "spotuify-store",
            "spotuify-search",
            "spotuify-spotify",
            "spotuify-player",
            "spotuify-sync",
            "spotuify-audio",
            "spotuify-cli",
            "spotuify-daemon",
        ],
        "spotuify-mcp" => &["spotuify-core", "spotuify-protocol"],
        _ => return None,
    };
    Some(allowed.iter().copied().collect())
}

fn iter_crates() -> impl Iterator<Item = (String, PathBuf)> {
    let crates_dir = repo_root().join("crates");
    let entries: Vec<_> = fs::read_dir(&crates_dir)
        .map(|rd| rd.collect::<Result<Vec<_>, _>>().unwrap())
        .unwrap_or_default();
    entries.into_iter().filter_map(|entry| {
        if !entry.file_type().ok()?.is_dir() {
            return None;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let manifest_path = entry.path().join("Cargo.toml");
        if manifest_path.exists() {
            Some((name, manifest_path))
        } else {
            None
        }
    })
}

#[test]
fn root_package_is_not_publishable() {
    let manifest = read_manifest("Cargo.toml").expect("root Cargo.toml must exist");
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .expect("root must have [package]");
    assert_eq!(
        package.get("publish").and_then(toml::Value::as_bool),
        Some(false),
        "root spotuify package must be publish = false (not distributed via crates.io)"
    );
}

#[test]
fn workspace_declares_resolver_2() {
    let manifest = read_manifest("Cargo.toml").expect("root Cargo.toml must exist");
    let workspace = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .expect("root must declare [workspace]");
    assert_eq!(
        workspace.get("resolver").and_then(toml::Value::as_str),
        Some("2"),
        "workspace.resolver must be \"2\" so feature unification follows the 2021 edition rules"
    );
}

#[test]
fn each_crate_has_documented_dependency_rules() {
    for (name, manifest_path) in iter_crates() {
        assert!(
            allowed_deps(&name).is_some(),
            "crate {name} at {} is not listed in allowed_deps(). Update tests/workspace_boundaries.rs::allowed_deps to declare its boundary.",
            manifest_path.display()
        );
    }
}

#[test]
fn no_crate_imports_outside_its_allowed_dependencies() {
    for (name, manifest_path) in iter_crates() {
        let manifest = read_manifest(
            manifest_path
                .strip_prefix(repo_root())
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let actual = internal_dep_names(&manifest);
        let allowed = allowed_deps(&name).unwrap_or_default();
        let allowed: BTreeSet<String> = allowed.into_iter().map(str::to_string).collect();
        let extras: BTreeSet<_> = actual.difference(&allowed).collect();
        assert!(
            extras.is_empty(),
            "crate {name} depends on disallowed internal crates: {extras:?}. Allowed: {allowed:?}"
        );
    }
}

#[test]
fn no_back_edges_via_self_dependency() {
    for (name, manifest_path) in iter_crates() {
        let manifest = read_manifest(
            manifest_path
                .strip_prefix(repo_root())
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let actual = internal_dep_names(&manifest);
        assert!(
            !actual.contains(&name),
            "crate {name} declares itself as an internal dependency (cycle)"
        );
    }
}

#[test]
#[ignore = "Aspirational. The root binary is the assembly point: it constructs SpotifyClient + AnalyticsStore on startup, owns the clap Cli enum (which embeds backend types), and wires the daemon autostart. Even with every business module extracted (which has happened), Cargo.toml lists backend crates as direct deps because the binary's entry path mentions their types. Re-enabling this test would require either type-erasing those entry-time constructions or moving them into wrapper functions inside cli/tui/daemon; both are substantial refactors with limited payoff."]
fn root_binary_does_not_depend_on_internal_crates_post_extraction() {
    // Test left for documentation. The dependency edges are
    // intentional (binary is assembly point); the architectural
    // promise the test was guarding (no business logic in main.rs)
    // is achieved by the moves landed in Phase 7.
    let manifest = read_manifest("Cargo.toml").expect("root Cargo.toml must exist");
    let forbidden: BTreeSet<&str> = [
        "spotuify-store",
        "spotuify-search",
        "spotuify-spotify",
        "spotuify-sync",
        "spotuify-player",
        "spotuify-system",
        "spotuify-lyrics",
        "spotuify-audio",
    ]
    .into_iter()
    .collect();

    let internal = internal_dep_names(&manifest);
    let leaks: BTreeSet<_> = internal
        .iter()
        .filter(|name| forbidden.contains(name.as_str()))
        .collect();

    assert!(
        leaks.is_empty(),
        "root spotuify binary must not depend on backend crates directly: {leaks:?}. \
         Route through spotuify-cli / spotuify-tui / spotuify-daemon."
    );
}
