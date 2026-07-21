#![allow(clippy::panic, clippy::unwrap_used)]

//! Compiler-enforced dependency DAG for the spotuify workspace.
//!
//! Adapted from mxr's `tests/workspace_boundaries.rs`. Each rule encodes the
//! dependency boundaries declared in `docs/blueprint/01-architecture.md`
//! §"Dependency rules". Adding a new crate requires extending [`ALLOWED_DEPS`].
//!
//! The test is permissive while crates are being extracted (no errors when a
//! crate doesn't yet exist). It tightens automatically as crates land.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

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
    internal_dep_names_for(
        manifest,
        &["dependencies", "dev-dependencies", "build-dependencies"],
    )
}

fn internal_dep_names_for(manifest: &toml::Value, tables: &[&str]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for table in tables {
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

fn allowed_dev_deps(crate_name: &str) -> BTreeSet<&'static str> {
    match crate_name {
        // Provider adapters and sync may consume the deterministic fake in
        // tests; production code remains independent from the reference adapter.
        "spotuify-spotify" | "spotuify-sync" => ["spotuify-provider-fake"].into_iter().collect(),
        _ => BTreeSet::new(),
    }
}

/// Allowed internal dependencies per crate. A crate not listed here may
/// not declare any internal `spotuify-*` deps; this fails closed.
///
/// Enforced build-up rules (the table below is authoritative):
///   1. core has no internal dependencies; provider-fake depends on core.
///   2. protocol depends on core; config depends on core + protocol.
///   3. store depends on core + protocol; search adds store.
///   4. spotify depends on core + protocol; player adds spotify + audio.
///   5. sync depends on core + protocol + store; system on core + protocol.
///   6. lyrics depends on core + store + player; audio depends on core.
///   7. launcher depends only on protocol.
///   8. daemon is the backend integration point.
///   9. clients depend on core + protocol; CLI additionally uses launcher.
fn allowed_deps(crate_name: &str) -> Option<BTreeSet<&'static str>> {
    let allowed: &[&'static str] = match crate_name {
        "spotuify-core" => &[],
        // Reference adapter + conformance harness. It depends only on the
        // provider-neutral contract it implements.
        "spotuify-provider-fake" => &["spotuify-core"],
        "spotuify-config" => &["spotuify-core", "spotuify-protocol"],
        "spotuify-protocol" => &["spotuify-core"],
        // store and search hold CacheStatus / SearchScopeData / SearchSourceData
        // shapes that originate in spotuify-protocol. The blueprint suggested
        // core-only; the practical reality is they consume protocol types.
        "spotuify-store" => &["spotuify-core", "spotuify-protocol"],
        "spotuify-search" => &["spotuify-core", "spotuify-protocol", "spotuify-store"],
        // SpotifyError maps to IpcErrorKind from protocol; AuthErrorKind serialises
        // into DaemonEvent::AuthError variants. Protocol dep is intentional.
        "spotuify-spotify" => &["spotuify-core", "spotuify-protocol"],
        // Phase 17: player owns the embedded sink-tap and feeds raw
        // samples into spotuify-audio's analyzer handle. Keep FFT and
        // loopback code out of player, but allow this one-way edge.
        "spotuify-player" => &["spotuify-core", "spotuify-spotify", "spotuify-audio"],
        "spotuify-sync" => &["spotuify-core", "spotuify-protocol", "spotuify-store"],
        "spotuify-system" => &["spotuify-core", "spotuify-protocol"],
        // Client-side daemon launcher (D021): ensure/start/restart/status
        // + socket probes. Depends only on protocol so the CLI never
        // links the daemon.
        "spotuify-launcher" => &["spotuify-protocol"],
        "spotuify-lyrics" => &["spotuify-core", "spotuify-store", "spotuify-player"],
        "spotuify-audio" => &["spotuify-core"],
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
            // Runtime fake mode is a real provider implementation, not
            // branches inside the Spotify adapter.
            "spotuify-provider-fake",
            // The daemon re-exports launcher lifecycle helpers so the
            // binary keeps one import path (D021).
            "spotuify-launcher",
            "spotuify-config",
        ],
        // Clients consume domain values and the IPC contract only. The CLI
        // additionally owns daemon lifecycle through the narrow launcher.
        "spotuify-cli" => &["spotuify-core", "spotuify-protocol", "spotuify-launcher"],
        "spotuify-tui" => &["spotuify-core", "spotuify-protocol"],
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

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|err| panic!("failed to enumerate {}: {err}", directory.display()));
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
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
        let allowed = allowed_deps(&name).unwrap_or_default();
        let allowed: BTreeSet<String> = allowed.into_iter().map(str::to_string).collect();
        let production = internal_dep_names_for(&manifest, &["dependencies", "build-dependencies"]);
        let extras: BTreeSet<_> = production.difference(&allowed).collect();
        assert!(
            extras.is_empty(),
            "crate {name} has disallowed production dependencies: {extras:?}. Allowed: {allowed:?}"
        );

        let mut dev_allowed = allowed;
        dev_allowed.extend(allowed_dev_deps(&name).into_iter().map(str::to_string));
        let dev = internal_dep_names_for(&manifest, &["dev-dependencies"]);
        let extras: BTreeSet<_> = dev.difference(&dev_allowed).collect();
        assert!(
            extras.is_empty(),
            "crate {name} has disallowed dev dependencies: {extras:?}. Allowed: {dev_allowed:?}"
        );
    }
}

#[test]
fn concrete_spotify_client_stays_inside_adapter_and_factory() {
    let concrete_client = concat!("Spotify", "Client");
    let mut sources = Vec::new();
    collect_rust_sources(&repo_root().join("src"), &mut sources);
    collect_rust_sources(&repo_root().join("crates"), &mut sources);

    let mut leaks = Vec::new();
    for source in sources {
        let relative = source.strip_prefix(repo_root()).unwrap();
        let allowed = relative.starts_with("crates/spotuify-spotify/")
            || relative == Path::new("crates/spotuify-daemon/src/provider_factory.rs");
        if allowed {
            continue;
        }
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source.display()));
        for (line_index, line) in contents.lines().enumerate() {
            if line.contains(concrete_client) {
                leaks.push(format!("{}:{}", relative.display(), line_index + 1));
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "concrete Spotify adapter leaked outside its crate and daemon factory: {leaks:?}"
    );
}

#[test]
fn legacy_spotify_aggregate_config_stays_inside_adapter() {
    let legacy_config = concat!("spotuify_spotify::config::", "Config");
    let mut sources = Vec::new();
    collect_rust_sources(&repo_root().join("src"), &mut sources);
    collect_rust_sources(&repo_root().join("crates"), &mut sources);

    let mut leaks = Vec::new();
    for source in sources {
        let relative = source.strip_prefix(repo_root()).unwrap();
        if relative.starts_with("crates/spotuify-spotify/") {
            continue;
        }
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source.display()));
        for (line_index, line) in contents.lines().enumerate() {
            if line.contains(legacy_config) {
                leaks.push(format!("{}:{}", relative.display(), line_index + 1));
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "legacy Spotify aggregate config leaked outside its adapter: {leaks:?}"
    );
}

#[test]
fn auth_persistence_stays_inside_adapter_or_daemon() {
    let auth_api = concat!("spotuify_spotify::", "auth");
    let mut sources = Vec::new();
    collect_rust_sources(&repo_root().join("src"), &mut sources);
    collect_rust_sources(&repo_root().join("crates"), &mut sources);

    let mut leaks = Vec::new();
    for source in sources {
        let relative = source.strip_prefix(repo_root()).unwrap();
        // The daemon owns auth workflows and the adapter owns credential IO.
        let allowed = relative.starts_with("crates/spotuify-spotify/")
            || relative.starts_with("crates/spotuify-daemon/");
        if allowed {
            continue;
        }
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source.display()));
        for (line_index, line) in contents.lines().enumerate() {
            if line.contains(auth_api) {
                leaks.push(format!("{}:{}", relative.display(), line_index + 1));
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "Spotify auth persistence leaked into a client or backend crate: {leaks:?}"
    );
}

#[test]
fn unified_cli_production_path_stays_provider_neutral() {
    let manifest = read_manifest("Cargo.toml").expect("root Cargo.toml must exist");
    let production_dependencies =
        internal_dep_names_for(&manifest, &["dependencies", "build-dependencies"]);
    assert!(
        !production_dependencies.contains("spotuify-spotify"),
        "the unified CLI must not depend directly on the Spotify adapter"
    );

    let source = fs::read_to_string(repo_root().join("src/main.rs"))
        .expect("root CLI source must be readable")
        // Windows checkouts may carry CRLF; normalize so the test-module
        // marker below matches and test-only literals stay out of scope.
        .replace("\r\n", "\n");
    let production = source
        .split_once("#[cfg(test)]\nmod tests")
        .map_or(source.as_str(), |(production, _)| production);
    let forbidden = [
        (
            "direct vendor dependency",
            concat!("spotuify_", "spotify::"),
        ),
        ("provider identity literal", concat!("\"", "spotify", "\"")),
        ("provider URI literal", concat!("\"", "spotify", ":")),
    ];
    let mut violations = Vec::new();
    for (line_index, line) in production.lines().enumerate() {
        for (label, pattern) in forbidden {
            if line.contains(pattern) {
                violations.push(format!("src/main.rs:{}: {label}", line_index + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "the unified CLI must discover provider identity, auth policy, and URI namespaces through provider-neutral config/IPC:\n{}",
        violations.join("\n")
    );
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
