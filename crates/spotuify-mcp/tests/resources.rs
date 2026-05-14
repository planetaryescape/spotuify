//! MCP resource catalogue tests.

use spotuify_mcp::resources::{resource_uris_invalidated_by, ResourceCatalogue};

#[test]
fn all_resources_have_spotuify_scheme() {
    for r in ResourceCatalogue::all() {
        assert!(
            r.uri.starts_with("spotuify://"),
            "uri {} not under spotuify://",
            r.uri
        );
    }
}

#[test]
fn resource_uris_are_unique() {
    let mut uris: Vec<&str> = ResourceCatalogue::all().iter().map(|r| r.uri).collect();
    uris.sort();
    let mut dedup = uris.clone();
    dedup.dedup();
    assert_eq!(uris, dedup);
}

#[test]
fn by_uri_lookup_round_trips() {
    for r in ResourceCatalogue::all() {
        assert_eq!(ResourceCatalogue::by_uri(r.uri), Some(r));
    }
    assert!(ResourceCatalogue::by_uri("spotuify://missing").is_none());
}

#[test]
fn playback_changed_event_invalidates_playback_resource() {
    let uris = resource_uris_invalidated_by("playback-changed");
    assert_eq!(uris, vec!["spotuify://playback"]);
}

#[test]
fn devices_changed_event_invalidates_devices_resource() {
    let uris = resource_uris_invalidated_by("devices-changed");
    assert_eq!(uris, vec!["spotuify://devices"]);
}

#[test]
fn playlists_changed_event_invalidates_playlists_resource() {
    let uris = resource_uris_invalidated_by("playlists-changed");
    assert_eq!(uris, vec!["spotuify://playlists"]);
}

#[test]
fn library_changed_event_invalidates_playlists_resource() {
    // Library = saved tracks/albums; surfaced as part of the playlists
    // resource for simplicity until a dedicated library resource lands.
    let uris = resource_uris_invalidated_by("library-changed");
    assert_eq!(uris, vec!["spotuify://playlists"]);
}

#[test]
fn unknown_event_invalidates_nothing() {
    let uris = resource_uris_invalidated_by("not-a-real-event");
    assert!(uris.is_empty());
}

#[test]
fn lyrics_resource_documents_phase_dependency() {
    let r = ResourceCatalogue::by_uri("spotuify://now_playing/lyrics").unwrap();
    assert!(
        r.description.contains("Phase 9"),
        "lyrics resource should call out Phase 9 dep: {}",
        r.description
    );
}

#[test]
fn subscribable_resources_have_event_mapping_implied() {
    // Every subscribable resource should be reachable from at least one
    // event tag (otherwise it can't actually update). Doctor is the one
    // exception (subscribable=false).
    for r in ResourceCatalogue::all() {
        if !r.subscribable {
            continue;
        }
        // Find any event tag whose invalidation list contains this uri.
        let known_events = [
            "playback-changed",
            "devices-changed",
            "playlists-changed",
            "library-changed",
        ];
        let mapped = known_events
            .iter()
            .any(|tag| resource_uris_invalidated_by(tag).contains(&r.uri));
        if r.uri == "spotuify://now_playing/lyrics" {
            // Lyrics has no event mapping yet -- gated on Phase 9/16. Skip.
            continue;
        }
        assert!(
            mapped,
            "subscribable resource {} has no event mapping",
            r.uri
        );
    }
}
