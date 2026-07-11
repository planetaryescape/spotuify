//! Behavior tests: bodyless mutation endpoints must produce a request
//! that Spotify's edge layer accepts.
//!
//! Spotify periodically rejects bodyless `PUT`/`POST` requests with
//! HTTP 411 "Length Required" even when the client sets
//! `Content-Length: 0` explicitly (reqwest issue #838 — the header is
//! occasionally stripped by middleware). The reliable contract is to
//! send a small JSON-object body (`{}`) with `Content-Type:
//! application/json`. These tests pin that contract via a strict
//! wiremock simulator that mirrors what Spotify's edge actually
//! demands in production.
//!
//! The mocks below match only when the request body parses as a JSON
//! object. If the implementation reverts to sending an empty body the
//! mock returns the default 404 and the call fails — that's how the
//! test catches a regression.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use spotuify_core::{MediaItem, MediaKind};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::client::{SpotifyClient, WebApiBearerProvider};
use spotuify_spotify::config::{
    AnalyticsConfig, CacheConfig, Config, DiscordConfig, NotificationsConfig, PlayerConfig,
    VizConfig,
};
use spotuify_spotify::SpotifyResult;
use tokio::sync::Mutex;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config() -> Config {
    Config {
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
    }
}

fn full_scope_token_cache() -> Arc<Mutex<Option<StoredToken>>> {
    Arc::new(Mutex::new(Some(StoredToken {
        access_token: "test-access".to_string(),
        refresh_token: "test-refresh".to_string(),
        expires_at: 4_000_000_000,
        scope: "user-modify-playback-state user-library-modify user-library-read \
                user-follow-modify user-follow-read user-read-playback-state \
                playlist-modify-public playlist-modify-private playlist-read-private \
                playlist-read-collaborative user-read-private user-read-recently-played \
                user-read-playback-position user-read-currently-playing"
            .to_string(),
        token_type: "Bearer".to_string(),
    })))
}

async fn test_client(server: &MockServer) -> SpotifyClient {
    SpotifyClient::new(test_config())
        .expect("test client should build")
        .with_api_base_for_tests(format!("{}/v1", server.uri()))
        .with_token_cache(full_scope_token_cache())
}

#[derive(Default)]
struct RecordingBearer {
    calls: Mutex<Vec<bool>>,
}

#[async_trait::async_trait]
impl WebApiBearerProvider for RecordingBearer {
    async fn bearer(&self, force_refresh: bool) -> SpotifyResult<String> {
        self.calls.lock().await.push(force_refresh);
        Ok(if force_refresh {
            "fresh-token"
        } else {
            "stale-token"
        }
        .to_string())
    }
}

async fn test_client_with_bearer_provider(
    server: &MockServer,
    provider: Arc<RecordingBearer>,
) -> SpotifyClient {
    SpotifyClient::new(test_config())
        .expect("test client should build")
        .with_api_base_for_tests(format!("{}/v1", server.uri()))
        .with_bearer_provider(provider)
}

fn track_item(uri: &str) -> MediaItem {
    let id = uri.rsplit(':').next().map(str::to_string);
    MediaItem {
        id,
        uri: uri.to_string(),
        name: "Test Track".to_string(),
        subtitle: "Test Artist".to_string(),
        context: "Test Album".to_string(),
        duration_ms: 180_000,
        image_url: None,
        kind: MediaKind::Track,
        source: Some("test".to_string()),
        freshness: None,
        explicit: Some(false),
        is_playable: Some(true),
        ..Default::default()
    }
}

#[tokio::test]
async fn pause_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/pause"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .play_pause(true)
        .await
        .expect("pause should succeed when request body is a JSON object");
}

#[tokio::test]
async fn seek_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/seek"))
        .and(query_param("position_ms", "30000"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .seek(30_000)
        .await
        .expect("seek should succeed when request body is a JSON object");
}

#[tokio::test]
async fn save_track_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/tracks"))
        .and(query_param("ids", "t1"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .save_item(&track_item("spotify:track:t1"))
        .await
        .expect("save_item should succeed when request body is a JSON object");
}

#[tokio::test]
async fn queue_append_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/player/queue"))
        .and(query_param("uri", "spotify:track:queued"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .add_to_queue("spotify:track:queued")
        .await
        .expect("add_to_queue should succeed when request body is a JSON object");
}

#[tokio::test]
async fn queue_append_retries_once_with_fresh_bearer_after_auth_expiry() {
    let server = MockServer::start().await;
    let provider = Arc::new(RecordingBearer::default());

    Mock::given(method("POST"))
        .and(path("/v1/me/player/queue"))
        .and(query_param("uri", "spotify:track:queued"))
        .and(header("authorization", "Bearer stale-token"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/me/player/queue"))
        .and(query_param("uri", "spotify:track:queued"))
        .and(header("authorization", "Bearer fresh-token"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client_with_bearer_provider(&server, provider.clone()).await;
    client
        .add_to_queue("spotify:track:queued")
        .await
        .expect("mutation should retry once with a forced fresh bearer after 401");

    let calls = provider.calls.lock().await.clone();
    assert_eq!(calls, vec![false, true]);
}

#[tokio::test]
async fn unlike_track_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v1/me/tracks"))
        .and(query_param("ids", "t1"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .library_unsave_by_uri("spotify:track:t1")
        .await
        .expect("unsave should succeed when request body is a JSON object");
}

#[tokio::test]
async fn skip_next_request_carries_json_object_body_so_spotify_edge_accepts_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/player/next"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = test_client(&server).await;
    client
        .next()
        .await
        .expect("next should succeed when request body is a JSON object");
}

#[tokio::test]
async fn search_with_limit_fans_per_type_requests_and_dedupes_results() {
    // Adversarial: concurrent fanout must hit /search once per media
    // kind (since Spotify rejects limit > 20 on multi-type queries),
    // and the merged result must dedupe by URI so a track surfaced
    // in both `track` and `album` responses isn't repeated.
    let server = MockServer::start().await;

    let shared_uri = "spotify:track:dual";
    let track_only_uri = "spotify:track:only";
    let album_only_uri = "spotify:album:only";

    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .and(query_param("type", "track"))
        .and(query_param("q", "jazz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tracks": {
                "href": "",
                "limit": 5,
                "offset": 0,
                "total": 2,
                "items": [
                    {
                        "id": "dual",
                        "uri": shared_uri,
                        "name": "Dual Track",
                        "duration_ms": 180_000,
                        "artists": [{"id": "a1", "name": "Artist One", "uri": "spotify:artist:a1"}],
                        "album": {
                            "id": "alb1",
                            "name": "Album One",
                            "uri": "spotify:album:alb1",
                            "images": []
                        }
                    },
                    {
                        "id": "only",
                        "uri": track_only_uri,
                        "name": "Track Only",
                        "duration_ms": 200_000,
                        "artists": [{"id": "a2", "name": "Artist Two", "uri": "spotify:artist:a2"}],
                        "album": {
                            "id": "alb2",
                            "name": "Album Two",
                            "uri": "spotify:album:alb2",
                            "images": []
                        }
                    }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .and(query_param("type", "album"))
        .and(query_param("q", "jazz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "albums": {
                "href": "",
                "limit": 5,
                "offset": 0,
                "total": 2,
                "items": [
                    // Same URI as the track above — must dedupe.
                    {
                        "id": "dual",
                        "uri": shared_uri,
                        "name": "Dual Track",
                        "album_type": "single",
                        "artists": [{"id": "a1", "name": "Artist One", "uri": "spotify:artist:a1"}],
                        "images": []
                    },
                    {
                        "id": "alb-only",
                        "uri": album_only_uri,
                        "name": "Album Only",
                        "album_type": "album",
                        "artists": [{"id": "a3", "name": "Artist Three", "uri": "spotify:artist:a3"}],
                        "images": []
                    }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Immutable binding — proves search_with_limit takes `&self` now.
    let client = test_client(&server).await;
    let items = client
        .search_with_limit("jazz", &[MediaKind::Track, MediaKind::Album], 5)
        .await
        .expect("concurrent fanout should succeed against both mocks");

    let uris: Vec<&str> = items.iter().map(|i| i.uri.as_str()).collect();
    // Three unique URIs total. The shared one appears once (first
    // occurrence wins — from the track response).
    assert_eq!(uris.len(), 3, "expected 3 deduped items, got {uris:?}");
    assert!(uris.contains(&shared_uri));
    assert!(uris.contains(&track_only_uri));
    assert!(uris.contains(&album_only_uri));
}
