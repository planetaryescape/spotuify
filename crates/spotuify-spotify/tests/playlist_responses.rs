use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use spotuify_core::{MediaKind, ResourceUri};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::config::{
    AnalyticsConfig, CacheConfig, Config, DiscordConfig, NotificationsConfig, PlayerConfig,
    VizConfig,
};
use spotuify_spotify::SpotifyClient;
use tokio::sync::Mutex;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_client(server: &MockServer) -> SpotifyClient {
    let config = Config {
        client_id: "test-client-id".to_string(),
        client_secret: Some("test-client-secret".to_string()),
        redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
        config_path: PathBuf::from("test-spotuify.toml"),
        player: PlayerConfig::default(),
        cache: CacheConfig::default(),
        analytics: AnalyticsConfig::default(),
        notifications: NotificationsConfig::default(),
        discord: DiscordConfig::default(),
        viz: VizConfig::default(),
    };
    let token = StoredToken {
        access_token: "test-access".to_string(),
        refresh_token: "test-refresh".to_string(),
        expires_at: 4_000_000_000,
        scope: "playlist-read-private playlist-modify-private".to_string(),
        token_type: "Bearer".to_string(),
    };
    SpotifyClient::new(config)
        .expect("test client")
        .with_api_base_for_tests(format!("{}/v1", server.uri()))
        .with_token_cache(Arc::new(Mutex::new(Some(token))))
}

fn playlist(summary: Value) -> Value {
    let mut value = json!({
        "id": "playlist-1",
        "uri": "spotify:playlist:playlist-1",
        "name": "Test Playlist",
        "owner": {"display_name": "Test Owner"},
        "snapshot_id": "snapshot-1"
    });
    value
        .as_object_mut()
        .expect("playlist object")
        .extend(summary.as_object().expect("summary object").clone());
    value
}

#[tokio::test]
async fn listing_accepts_legacy_current_and_dual_playlist_summaries() {
    let cases = [
        (json!({"tracks": {"total": 7}}), 7),
        (json!({"items": {"total": 8}}), 8),
        (json!({"tracks": {"total": 9}, "items": {"total": 9}}), 9),
        (json!({"tracks": {"total": 10}, "items": {"total": 11}}), 11),
        (json!({"tracks": {"total": 12}, "items": {"total": 0}}), 0),
        (json!({"tracks": {"total": 13}, "items": null}), 13),
        (json!({"tracks": null, "items": {"total": 14}}), 14),
        (json!({"tracks": null, "items": null}), 0),
        (json!({}), 0),
    ];
    for (summary, expected_total) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total": 2,
                "items": [null, playlist(summary.clone())]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let page = test_client(&server)
            .playlists_page(50, 0)
            .await
            .expect("playlist summary should deserialize");

        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "playlist-1");
        assert_eq!(page.items[0].owner, "Test Owner");
        assert_eq!(page.items[0].tracks_total, expected_total, "{summary}");
        assert_eq!(page.items[0].version_token.as_deref(), Some("snapshot-1"));
    }
}

#[tokio::test]
async fn metadata_accepts_dual_playlist_summaries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/playlists/playlist-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(playlist(json!({
            "tracks": {"total": 5}, "items": {"total": 6}
        }))))
        .expect(1)
        .mount(&server)
        .await;

    let result = test_client(&server)
        .playlist_metadata("playlist-1")
        .await
        .expect("playlist metadata with both summary fields");

    assert_eq!(result.id, "playlist-1");
    assert_eq!(result.tracks_total, 6);
    assert_eq!(result.version_token.as_deref(), Some("snapshot-1"));
}

#[tokio::test]
async fn search_accepts_dual_playlist_summaries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "playlists": {
                "total": 1,
                "items": [playlist(json!({"tracks": {"total": 5}, "items": {"total": 6}}))]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = test_client(&server)
        .search_page("Test Playlist", MediaKind::Playlist, 0)
        .await
        .expect("playlist search with both summary fields");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].uri, "spotify:playlist:playlist-1");
    assert_eq!(result[0].context, "6 tracks");
}

#[tokio::test]
async fn creation_accepts_dual_playlist_summaries_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/playlists"))
        .and(body_json(json!({
            "name": "Test Playlist",
            "description": "Test description",
            "public": false
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(playlist(json!({
            "tracks": {"total": 0}, "items": {"total": 0}
        }))))
        .expect(1)
        .mount(&server)
        .await;

    let result = test_client(&server)
        .create_playlist("Test Playlist", Some("Test description"), false)
        .await
        .expect("created playlist with both summary fields");

    assert_eq!(result.id, "playlist-1");
    assert_eq!(result.name, "Test Playlist");
    assert_eq!(result.tracks_total, 0);
    assert_eq!(result.version_token.as_deref(), Some("snapshot-1"));
}

#[tokio::test]
async fn playlist_entries_accept_legacy_current_and_dual_playable_fields() {
    let track = json!({
        "type": "track",
        "id": "track-1",
        "uri": "spotify:track:track-1",
        "name": "Test Track",
        "duration_ms": 1000,
        "artists": [{"name": "Test Artist"}],
        "album": {"name": "Test Album"}
    });
    let episode = json!({
        "type": "episode",
        "id": "episode-1",
        "uri": "spotify:episode:episode-1",
        "name": "Test Episode",
        "duration_ms": 2000,
        "show": {"name": "Test Show", "publisher": "Test Publisher"}
    });
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/playlists/playlist-1/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 18,
            "items": [
                {"track": track},
                {"item": track},
                {"track": track, "item": track},
                {"track": track, "item": episode},
                {"track": track, "item": null},
                {"track": null, "item": episode},
                {"track": null, "item": null},
                {}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = test_client(&server)
        .playlist_tracks_page("playlist-1", 50, 10)
        .await
        .expect("playlist entries with both playable fields");

    assert_eq!(page.total, 18);
    assert_eq!(page.offset, 10);
    assert_eq!(page.items.len(), 8);
    for index in [0, 1, 2, 4] {
        assert_eq!(page.items[index].uri, "spotify:track:track-1");
        assert_eq!(page.items[index].name, "Test Track");
        assert_eq!(page.items[index].kind, MediaKind::Track);
    }
    for index in [3, 5] {
        assert_eq!(page.items[index].uri, "spotify:episode:episode-1");
        assert_eq!(page.items[index].name, "Test Episode");
        assert_eq!(page.items[index].kind, MediaKind::Episode);
    }
    for index in [6, 7] {
        let expected_uri = ResourceUri::spotify(
            MediaKind::Track,
            format!("unavailable~playlist-1~{}", 10 + index),
        )
        .expect("unavailable entry URI");
        assert_eq!(page.items[index].uri, expected_uri.as_uri());
        assert_eq!(page.items[index].is_playable, Some(false));
    }
}
