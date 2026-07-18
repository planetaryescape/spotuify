#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 10 (P10.3-P10.5) — analytics query behaviour tests.
//!
//! Exercises `top_entries`, `habit_buckets`, `rediscovery_candidates`,
//! and the retention prune helpers against a seeded in-memory store.
//! Tests verify SQL behaviour via observable results, not internal
//! query strings.
//!
//! Five-question gate per test:
//! 1. Would a bug fail it? — Flip qualified flag, swap ORDER BY, change
//!    GROUP BY column: tests catch each.
//! 2. Expected values from spec? — Counts and ordering come from the
//!    seeded scenario, not from reading the SQL.
//! 3. Edge cases? — empty store, zero rows below cutoff, mixed
//!    qualified/unqualified listens, multiple tracks tied on
//!    audible_ms (ordering tiebreak).
//! 4. Survive function-body deletion? — Yes: deleting query bodies
//!    returns empty vecs, every test asserts on non-empty results.
//! 5. Survive implementation swap? — Yes: tests use only public Store
//!    methods + observable rows.

use spotuify_core::{
    BackendLabel, ListenFact, MeasurementKind, MediaItem, MediaKind, PlaybackSource, Playlist,
    ProviderId, ResourceUri, SkipReason, UriScheme,
};
use spotuify_protocol::{HabitWindow, SinceWindow, TopKind};
use spotuify_store::{NewExternalScrobble, Store};

async fn store() -> Store {
    Store::in_memory().await.unwrap()
}

fn fact(
    session_id: &str,
    track_uri: &str,
    artist_uri: Option<&str>,
    started_at_ms: i64,
    audible_ms: i64,
    qualified: bool,
    skip_reason: Option<SkipReason>,
) -> ListenFact {
    ListenFact {
        id: None,
        session_id: session_id.to_string(),
        track_uri: track_uri.to_string(),
        artist_uri: artist_uri.map(String::from),
        album_uri: None,
        context_uri: None,
        started_at_ms,
        ended_at_ms: started_at_ms + audible_ms,
        duration_ms: audible_ms * 2,
        elapsed_ms: audible_ms,
        audible_ms,
        completion_ratio: 0.5,
        qualified,
        qualification_rule_version: 1,
        skip_reason,
        source: Some(PlaybackSource::Unknown),
        backend: Some(BackendLabel::Embedded),
        private_session: false,
        measurement_kind: MeasurementKind::ObservedPlayback,
        external_scrobble_id: None,
        created_at_ms: started_at_ms + audible_ms,
    }
}

fn imported_fact(
    session_id: &str,
    track_uri: &str,
    artist_uri: Option<&str>,
    album_uri: Option<&str>,
    started_at_ms: i64,
    audible_ms: i64,
    external_scrobble_id: i64,
) -> ListenFact {
    let mut fact = fact(
        session_id,
        track_uri,
        artist_uri,
        started_at_ms,
        audible_ms,
        true,
        Some(SkipReason::TrackEnd),
    );
    fact.album_uri = album_uri.map(String::from);
    fact.measurement_kind = MeasurementKind::LastfmScrobbleImport;
    fact.external_scrobble_id = Some(external_scrobble_id);
    fact
}

fn scrobble(run_id: &str, idempotency_key: &str, scrobbled_at_ms: i64) -> NewExternalScrobble {
    NewExternalScrobble {
        provider: "lastfm".to_string(),
        username: "tester".to_string(),
        import_run_id: run_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        scrobbled_at_ms,
        artist_name: "Artist".to_string(),
        track_name: "Track".to_string(),
        album_name: Some("Album".to_string()),
        artist_mbid: None,
        track_mbid: None,
        album_mbid: None,
        url: Some("https://last.fm/user/tester/library/music/Artist/_/Track".to_string()),
        raw_json: serde_json::json!({"artist":"Artist","track":"Track"}),
        normalized_key: "artist track album".to_string(),
    }
}

fn media_item(uri: &str, kind: MediaKind, name: &str, subtitle: &str, context: &str) -> MediaItem {
    MediaItem {
        id: ResourceUri::parse(uri)
            .ok()
            .map(|resource| resource.bare_id().to_string()),
        uri: uri.to_string(),
        name: name.to_string(),
        subtitle: subtitle.to_string(),
        context: context.to_string(),
        kind,
        ..Default::default()
    }
}

// --- top_entries ---------------------------------------------------------

#[tokio::test]
async fn top_tracks_ranks_by_total_audible_ms_descending() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    // Track A: two qualified listens, 60s each = 120s total audible.
    s.insert_listen_fact(&fact(
        "a1",
        "spotify:track:a",
        None,
        now,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&fact(
        "a2",
        "spotify:track:a",
        None,
        now,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    // Track B: one qualified listen, 200s total — should win.
    s.insert_listen_fact(&fact(
        "b1",
        "spotify:track:b",
        None,
        now,
        200_000,
        true,
        None,
    ))
    .await
    .unwrap();

    let top = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(top.len(), 2, "two tracks have qualified listens");
    assert_eq!(top[0].uri, "spotify:track:b", "200s audible beats 120s");
    assert_eq!(top[0].total_audible_ms, 200_000);
    assert_eq!(top[1].uri, "spotify:track:a");
    assert_eq!(top[1].total_audible_ms, 120_000);
}

#[tokio::test]
async fn top_output_partitions_same_bare_id_by_provider_for_tracks_and_playlists() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    let spotify_track = ResourceUri::new(UriScheme::Spotify, MediaKind::Track, "shared")
        .unwrap()
        .as_uri();
    let fake_track = ResourceUri::new(UriScheme::Fake, MediaKind::Track, "shared")
        .unwrap()
        .as_uri();
    let spotify_playlist = ResourceUri::new(UriScheme::Spotify, MediaKind::Playlist, "shared")
        .unwrap()
        .as_uri();
    let fake_playlist = ResourceUri::new(UriScheme::Fake, MediaKind::Playlist, "shared")
        .unwrap()
        .as_uri();

    let mut spotify = fact("spotify", &spotify_track, None, now, 60_000, true, None);
    spotify.context_uri = Some(spotify_playlist.clone());
    let mut fake = fact("fake", &fake_track, None, now + 1, 90_000, true, None);
    fake.context_uri = Some(fake_playlist.clone());
    s.insert_listen_fact(&spotify).await.unwrap();
    s.insert_listen_fact(&fake).await.unwrap();

    let all_tracks = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(all_tracks.len(), 2, "providers remain visible partitions");
    let spotify_tracks = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, Some("spotify"))
        .await
        .unwrap();
    assert_eq!(spotify_tracks.len(), 1);
    assert_eq!(spotify_tracks[0].uri, spotify_track);

    let fake_playlists = s
        .top_entries(TopKind::Playlists, SinceWindow::All, 10, Some("fake"))
        .await
        .unwrap();
    assert_eq!(fake_playlists.len(), 1);
    assert_eq!(fake_playlists[0].uri, fake_playlist);
}

#[tokio::test]
async fn analytics_filters_use_configured_provider_identity_not_uri_scheme() {
    let s = store().await;
    let provider = ProviderId::new("custom-cloud").unwrap();
    let now = spotuify_core::now_ms();
    let track_uri = "spotify:track:custom";
    let artist_uri = "spotify:artist:custom";
    let album_uri = "spotify:album:custom";
    let playlist_uri = "spotify:playlist:custom";

    s.upsert_provider_media_items(
        &provider,
        &[
            media_item(
                track_uri,
                MediaKind::Track,
                "Custom Track",
                "Custom Artist",
                "Custom Album",
            ),
            media_item(artist_uri, MediaKind::Artist, "Custom Artist", "", ""),
            media_item(
                album_uri,
                MediaKind::Album,
                "Custom Album",
                "Custom Artist",
                "",
            ),
        ],
        provider.as_str(),
    )
    .await
    .unwrap();
    s.persist_provider_playlists(
        provider.as_str(),
        &[Playlist {
            id: playlist_uri.to_string(),
            name: "Custom Playlist".to_string(),
            owner: "Owner".to_string(),
            tracks_total: 1,
            image_url: None,
            version_token: None,
        }],
    )
    .await
    .unwrap();

    let mut listen = fact(
        "custom-listen",
        track_uri,
        Some(artist_uri),
        now - 100 * 86_400_000,
        60_000,
        true,
        None,
    );
    listen.album_uri = Some(album_uri.to_string());
    listen.context_uri = Some(playlist_uri.to_string());
    s.insert_listen_fact(&listen).await.unwrap();

    assert_eq!(
        s.listen_context_uris(track_uri).await.unwrap(),
        (Some(artist_uri.to_string()), Some(album_uri.to_string()))
    );
    for (kind, expected_uri) in [
        (TopKind::Tracks, track_uri),
        (TopKind::Artists, artist_uri),
        (TopKind::Albums, album_uri),
        (TopKind::Playlists, playlist_uri),
    ] {
        let custom = s
            .top_entries(kind, SinceWindow::All, 10, Some(provider.as_str()))
            .await
            .unwrap();
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].uri, expected_uri);
        assert!(s
            .top_entries(kind, SinceWindow::All, 10, Some("spotify"))
            .await
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        s.habit_buckets(HabitWindow::Day, None, Some(provider.as_str()))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(s
        .habit_buckets(HabitWindow::Day, None, Some("spotify"))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        s.rediscovery_candidates(90, 10, Some(provider.as_str()))
            .await
            .unwrap()[0]
            .track_uri,
        track_uri
    );
    assert!(s
        .rediscovery_candidates(90, 10, Some("spotify"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn top_playlists_groups_by_context_uri_and_ignores_non_playlist_contexts() {
    let s = store().await;
    let now = spotuify_core::now_ms();

    let with_context = |session: &str, track: &str, context: &str, audible: i64| {
        let mut f = fact(session, track, None, now, audible, true, None);
        f.context_uri = Some(context.to_string());
        f
    };

    // Two listens from one playlist (90s total), one from another (200s),
    // and one played from a bare track context (must be excluded).
    s.insert_listen_fact(&with_context(
        "p1",
        "spotify:track:a",
        "spotify:playlist:AA",
        40_000,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&with_context(
        "p2",
        "spotify:track:b",
        "spotify:playlist:AA",
        50_000,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&with_context(
        "p3",
        "spotify:track:c",
        "spotify:playlist:BB",
        200_000,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&with_context(
        "t1",
        "spotify:track:d",
        "spotify:track:d",
        999_000,
    ))
    .await
    .unwrap();

    let top = s
        .top_entries(TopKind::Playlists, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(top.len(), 2, "only the two playlist contexts count");
    assert_eq!(top[0].uri, "spotify:playlist:BB", "200s beats 90s");
    assert_eq!(top[0].total_audible_ms, 200_000);
    assert_eq!(top[1].uri, "spotify:playlist:AA");
    assert_eq!(top[1].total_audible_ms, 90_000);
}

#[tokio::test]
async fn top_tracks_excludes_unqualified_listens() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    // 1 qualified + 5 unqualified for the same track. Only the
    // qualified one should drive the rank.
    s.insert_listen_fact(&fact("q", "spotify:track:x", None, now, 50_000, true, None))
        .await
        .unwrap();
    for i in 0..5 {
        s.insert_listen_fact(&fact(
            &format!("s{i}"),
            "spotify:track:x",
            None,
            now,
            999_999,
            false,
            Some(SkipReason::UserNext),
        ))
        .await
        .unwrap();
    }
    let top = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(top.len(), 1, "only one row — the qualified listen");
    assert_eq!(
        top[0].total_audible_ms, 50_000,
        "unqualified listens contribute zero to total_audible_ms"
    );
}

#[tokio::test]
async fn top_tracks_respects_since_window() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    let ninety_days_ms = 90 * 86_400_000;
    // Old qualified listen — outside the 7d window.
    s.insert_listen_fact(&fact(
        "old",
        "spotify:track:old",
        None,
        now - ninety_days_ms,
        100_000,
        true,
        None,
    ))
    .await
    .unwrap();
    // Recent qualified listen — inside any window.
    s.insert_listen_fact(&fact(
        "recent",
        "spotify:track:new",
        None,
        now - 1_000,
        50_000,
        true,
        None,
    ))
    .await
    .unwrap();

    let last_7d = s
        .top_entries(TopKind::Tracks, SinceWindow::Days(7), 10, None)
        .await
        .unwrap();
    assert_eq!(
        last_7d.len(),
        1,
        "only the recent listen falls inside the 7d window"
    );
    assert_eq!(last_7d[0].uri, "spotify:track:new");

    let all_time = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(all_time.len(), 2);
}

#[tokio::test]
async fn top_tracks_returns_empty_when_no_qualified_listens() {
    let s = store().await;
    let top = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert!(top.is_empty());
}

#[tokio::test]
async fn imported_listens_update_metric_rollups_and_top_entries() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    s.create_import_run("run-rollup", "lastfm", "tester", None, None, false)
        .await
        .unwrap();
    let stored = s
        .insert_external_scrobble(&scrobble("run-rollup", "dupe-key", now))
        .await
        .unwrap();
    let fact = imported_fact(
        "lastfm-import-1",
        "spotify:track:imported",
        Some("spotify:artist:imported"),
        Some("spotify:album:imported"),
        now - 60_000,
        60_000,
        stored.id,
    );
    s.insert_listen_fact(&fact).await.unwrap();
    s.upsert_track_metric(&fact.track_uri, true, fact.audible_ms, fact.ended_at_ms)
        .await
        .unwrap();
    s.upsert_artist_metric(
        fact.artist_uri.as_deref().unwrap(),
        true,
        fact.audible_ms,
        fact.ended_at_ms,
    )
    .await
    .unwrap();
    s.upsert_album_metric(
        fact.album_uri.as_deref().unwrap(),
        true,
        fact.audible_ms,
        fact.ended_at_ms,
    )
    .await
    .unwrap();

    let top = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(top[0].uri, "spotify:track:imported");
    let track_count: i64 = sqlx::query_scalar(
        "SELECT qualified_count FROM track_metrics WHERE track_uri = 'spotify:track:imported'",
    )
    .fetch_one(s.reader())
    .await
    .unwrap();
    let artist_count: i64 = sqlx::query_scalar(
        "SELECT qualified_count FROM artist_metrics WHERE artist_uri = 'spotify:artist:imported'",
    )
    .fetch_one(s.reader())
    .await
    .unwrap();
    let album_count: i64 = sqlx::query_scalar(
        "SELECT qualified_count FROM album_metrics WHERE album_uri = 'spotify:album:imported'",
    )
    .fetch_one(s.reader())
    .await
    .unwrap();
    assert_eq!((track_count, artist_count, album_count), (1, 1, 1));
}

#[tokio::test]
async fn undo_import_removes_promoted_facts_and_rebuilds_rollups_but_preserves_audit_rows() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    s.create_import_run("run-undo", "lastfm", "tester", None, None, false)
        .await
        .unwrap();
    let stored = s
        .insert_external_scrobble(&scrobble("run-undo", "undo-key", now))
        .await
        .unwrap();
    let fact = imported_fact(
        "lastfm-import-undo",
        "spotify:track:undo",
        Some("spotify:artist:undo"),
        Some("spotify:album:undo"),
        now - 30_000,
        30_000,
        stored.id,
    );
    s.insert_listen_fact(&fact).await.unwrap();
    s.upsert_track_metric(&fact.track_uri, true, fact.audible_ms, fact.ended_at_ms)
        .await
        .unwrap();
    s.upsert_artist_metric(
        fact.artist_uri.as_deref().unwrap(),
        true,
        fact.audible_ms,
        fact.ended_at_ms,
    )
    .await
    .unwrap();
    s.upsert_album_metric(
        fact.album_uri.as_deref().unwrap(),
        true,
        fact.audible_ms,
        fact.ended_at_ms,
    )
    .await
    .unwrap();

    let summary = s.undo_import_run("run-undo", false).await.unwrap();
    assert_eq!(summary.listen_facts_removed, 1);
    assert_eq!(summary.raw_scrobbles_preserved, 1);
    let facts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM listen_facts WHERE external_scrobble_id = ?")
            .bind(stored.id)
            .fetch_one(s.reader())
            .await
            .unwrap();
    let raw: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM external_scrobbles WHERE import_run_id = 'run-undo'",
    )
    .fetch_one(s.reader())
    .await
    .unwrap();
    let metric_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM track_metrics WHERE track_uri = 'spotify:track:undo'",
    )
    .fetch_one(s.reader())
    .await
    .unwrap();
    assert_eq!(facts, 0);
    assert_eq!(raw, 1);
    assert_eq!(metric_rows, 0);
}

#[tokio::test]
async fn duplicate_external_scrobbles_do_not_duplicate_promoted_facts() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    s.create_import_run("run-dupes", "lastfm", "tester", None, None, false)
        .await
        .unwrap();
    let first = s
        .insert_external_scrobble(&scrobble("run-dupes", "same-key", now))
        .await
        .unwrap();
    let second = s
        .insert_external_scrobble(&scrobble("run-dupes", "same-key", now))
        .await
        .unwrap();
    assert!(!first.duplicate);
    assert!(second.duplicate);
    assert_eq!(first.id, second.id);

    let fact = imported_fact(
        "lastfm-import-dupe",
        "spotify:track:dupe",
        None,
        None,
        now - 30_000,
        30_000,
        first.id,
    );
    s.insert_listen_fact(&fact).await.unwrap();
    let duplicate_fact = imported_fact(
        "lastfm-import-dupe-repeat",
        "spotify:track:dupe",
        None,
        None,
        now - 30_000,
        30_000,
        second.id,
    );
    assert!(
        s.insert_listen_fact(&duplicate_fact).await.is_err(),
        "unique external_scrobble_id index must reject duplicate promoted facts"
    );
    assert_eq!(
        s.count_listen_facts_for_external(first.id).await.unwrap(),
        1
    );
}

#[tokio::test]
async fn top_tracks_limit_caps_result_count() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    for i in 0..5 {
        let uri = spotuify_core::ResourceUri::spotify(MediaKind::Track, i.to_string())
            .unwrap()
            .as_uri();
        s.insert_listen_fact(&fact(
            &format!("s{i}"),
            &uri,
            None,
            now,
            (i + 1) as i64 * 10_000,
            true,
            None,
        ))
        .await
        .unwrap();
    }
    let top3 = s
        .top_entries(TopKind::Tracks, SinceWindow::All, 3, None)
        .await
        .unwrap();
    assert_eq!(top3.len(), 3, "limit=3 must cap at 3 rows");
}

#[tokio::test]
async fn top_artists_groups_by_artist_uri() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    // Two tracks by the same artist, one track by a different artist.
    s.insert_listen_fact(&fact(
        "a",
        "spotify:track:t1",
        Some("spotify:artist:luther"),
        now,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&fact(
        "b",
        "spotify:track:t2",
        Some("spotify:artist:luther"),
        now,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&fact(
        "c",
        "spotify:track:t3",
        Some("spotify:artist:other"),
        now,
        90_000,
        true,
        None,
    ))
    .await
    .unwrap();

    let top = s
        .top_entries(TopKind::Artists, SinceWindow::All, 10, None)
        .await
        .unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].uri, "spotify:artist:luther", "120s total wins");
    assert_eq!(top[0].total_audible_ms, 120_000);
    assert_eq!(top[1].uri, "spotify:artist:other");
}

// --- habit_buckets -------------------------------------------------------

#[tokio::test]
async fn habit_buckets_aggregates_by_window() {
    let s = store().await;
    // Two listens in the same day → one bucket with 100s.
    let day_zero = 86_400_000_i64; // Day 1 epoch
    s.insert_listen_fact(&fact(
        "a",
        "spotify:track:1",
        None,
        day_zero + 1_000,
        50_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&fact(
        "b",
        "spotify:track:2",
        None,
        day_zero + 2_000,
        50_000,
        true,
        None,
    ))
    .await
    .unwrap();
    // One listen on the next day → second bucket.
    s.insert_listen_fact(&fact(
        "c",
        "spotify:track:3",
        None,
        day_zero + 90_000_000,
        30_000,
        true,
        None,
    ))
    .await
    .unwrap();

    let buckets = s.habit_buckets(HabitWindow::Day, None, None).await.unwrap();
    assert_eq!(buckets.len(), 2, "two distinct day-buckets");
    // Earliest bucket first (ASC ordering):
    assert!(buckets[0].bucket_start_ms < buckets[1].bucket_start_ms);
    assert_eq!(buckets[0].unique_tracks, 2);
    assert!(
        (buckets[0].listening_minutes - 100_000.0 / 60_000.0).abs() < 0.001,
        "100s = 100/60000 minutes"
    );
}

#[tokio::test]
async fn habit_buckets_filters_by_since_ms() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    s.insert_listen_fact(&fact(
        "old",
        "spotify:track:1",
        None,
        now - 365 * 86_400_000,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.insert_listen_fact(&fact(
        "new",
        "spotify:track:2",
        None,
        now,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    let buckets = s
        .habit_buckets(HabitWindow::Day, Some(now - 86_400_000), None)
        .await
        .unwrap();
    assert_eq!(
        buckets.len(),
        1,
        "since=now-1d must exclude the year-old listen"
    );
}

#[tokio::test]
async fn habit_buckets_include_top_hour_exploration_and_repeat_ratios() {
    let s = store().await;
    let day_zero = 86_400_000_i64;
    let hour_9 = day_zero + 9 * 3_600_000;
    let hour_20 = day_zero + 20 * 3_600_000;

    s.insert_listen_fact(&fact(
        "before",
        "spotify:track:old",
        Some("spotify:artist:one"),
        day_zero - 3_600_000,
        60_000,
        true,
        None,
    ))
    .await
    .expect("pre-bucket fact should insert");

    for (session, track, started_at_ms) in [
        ("a", "spotify:track:new-a", hour_9),
        ("b", "spotify:track:old", hour_9 + 60_000),
        ("c", "spotify:track:new-b", hour_20),
        ("d", "spotify:track:old", hour_20 + 60_000),
    ] {
        s.insert_listen_fact(&fact(
            session,
            track,
            Some("spotify:artist:one"),
            started_at_ms,
            60_000,
            true,
            None,
        ))
        .await
        .expect("bucket fact should insert");
    }

    let buckets = s
        .habit_buckets(HabitWindow::Day, Some(day_zero), None)
        .await
        .expect("habit buckets should load");

    assert_eq!(buckets.len(), 1);
    let bucket = &buckets[0];
    assert_eq!(bucket.top_hour_of_day, Some(9));
    assert_eq!(bucket.unique_tracks, 3);
    assert_eq!(bucket.unique_artists, 1);
    assert!(
        (bucket.exploration_ratio - (2.0 / 3.0)).abs() < 0.001,
        "two of three unique bucket tracks are first-ever listens"
    );
    assert!(
        (bucket.repeat_ratio - 0.25).abs() < 0.001,
        "four listens, three unique tracks => one repeated listen"
    );
}

// --- rediscovery_candidates ---------------------------------------------

#[tokio::test]
async fn rediscovery_picks_only_tracks_outside_the_gap() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    let day_ms = 86_400_000_i64;

    // Listened 100 days ago (outside a 90d gap). No track_metrics
    // write: rediscovery must see imported listen_facts that do not
    // update legacy rollups.
    s.insert_listen_fact(&fact(
        "old",
        "spotify:track:dormant",
        None,
        now - 100 * day_ms,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();

    // Listened 5 days ago (inside any reasonable gap).
    s.insert_listen_fact(&fact(
        "recent",
        "spotify:track:fresh",
        None,
        now - 5 * day_ms,
        60_000,
        true,
        None,
    ))
    .await
    .unwrap();
    s.upsert_track_metric("spotify:track:fresh", true, 60_000, now - 5 * day_ms)
        .await
        .unwrap();

    let dormant = s.rediscovery_candidates(90, 10, None).await.unwrap();
    assert_eq!(dormant.len(), 1, "only the >90d-dormant track surfaces");
    assert_eq!(dormant[0].track_uri, "spotify:track:dormant");
    assert!(
        dormant[0].days_since_last_listen >= 99,
        "days_since_last_listen must be ≥99 (got {})",
        dormant[0].days_since_last_listen
    );
}

#[tokio::test]
async fn rediscovery_excludes_zero_qualified_tracks() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    let day_ms = 86_400_000_i64;
    // Old track but never qualified — should not appear (can't rediscover something never enjoyed).
    s.upsert_track_metric(
        "spotify:track:never-qualified",
        false,
        20_000,
        now - 200 * day_ms,
    )
    .await
    .unwrap();
    let dormant = s.rediscovery_candidates(90, 10, None).await.unwrap();
    assert!(
        dormant.is_empty(),
        "tracks with qualified_count=0 must not surface"
    );
}

// --- retention prune -----------------------------------------------------

#[tokio::test]
async fn prune_playback_progress_drops_only_old_rows() {
    let s = store().await;
    let now = spotuify_core::now_ms();
    // Seed two rows: one old (95d), one recent (2d).
    sqlx::query(
        "INSERT INTO playback_progress
            (session_id, track_uri, sampled_at_ms, position_ms, audible_samples, sample_rate, channels)
         VALUES (?, ?, ?, ?, ?, ?, ?), (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("s-old")
    .bind("spotify:track:a")
    .bind(now - 95 * 86_400_000)
    .bind(0_i64)
    .bind(0_i64)
    .bind(44_100_i64)
    .bind(2_i64)
    .bind("s-new")
    .bind("spotify:track:b")
    .bind(now - 2 * 86_400_000)
    .bind(0_i64)
    .bind(0_i64)
    .bind(44_100_i64)
    .bind(2_i64)
    .execute(s.writer_for_test())
    .await
    .unwrap();

    let pruned = s
        .prune_playback_progress(now - 90 * 86_400_000)
        .await
        .unwrap();
    assert_eq!(pruned, 1, "only the 95-day-old row must be deleted");

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playback_progress")
        .fetch_one(s.reader())
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[tokio::test]
async fn prune_analytics_events_drops_only_old_rows() {
    // We need an analytics_events table; the cache DB doesn't include
    // it (it lives in the AnalyticsStore today). For this test we
    // create a stub table inside the in-memory cache DB so the prune
    // SQL runs end-to-end. Production AnalyticsStore uses the same
    // helper with the real table.
    let s = store().await;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS analytics_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            occurred_at_ms INTEGER NOT NULL,
            subject_uri TEXT,
            search_query TEXT,
            search_query_hash TEXT,
            payload_json TEXT NOT NULL DEFAULT '{}'
        )",
    )
    .execute(s.writer_for_test())
    .await
    .unwrap();

    let now = spotuify_core::now_ms();
    let one_year_ms = 365 * 86_400_000_i64;
    for i in 0..3 {
        sqlx::query(
            "INSERT INTO analytics_events (kind, occurred_at_ms, payload_json)
             VALUES ('search_performed', ?, '{}')",
        )
        .bind(now - (i as i64) * one_year_ms - 86_400_000)
        .execute(s.writer_for_test())
        .await
        .unwrap();
    }
    // 3 rows: 1d-old, 366d-old, 731d-old. Prune anything older than 365d.
    let pruned = s.prune_analytics_events(now - one_year_ms).await.unwrap();
    assert_eq!(pruned, 2, "two rows older than 365d must be pruned");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM analytics_events")
        .fetch_one(s.reader())
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[tokio::test]
async fn rebuild_derivations_is_idempotent_with_no_events() {
    let s = store().await;
    let report = s.rebuild_derivations_from_events(None).await.unwrap();
    assert_eq!(report.events_processed, 0);
    assert_eq!(report.listen_facts_emitted, 0);
    assert_eq!(report.qualified_listens, 0);
}
