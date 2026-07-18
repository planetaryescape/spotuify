#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::{json, Value};
use spotuify_core::{MediaKind, MusicProvider as _, ResourceUri};
use spotuify_provider_fake::{
    run_provider_conformance, run_transport_conformance, ConformanceFixtures, ConformanceOptions,
    LibraryFixture, PlaylistFixture, SearchFixture, TransportFixture,
};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::config::{
    AnalyticsConfig, CacheConfig, Config, DiscordConfig, NotificationsConfig, PlayerConfig,
    VizConfig,
};
use spotuify_spotify::SpotifyClient;
use tokio::sync::Mutex;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[derive(Clone)]
struct TestPlaylist {
    name: String,
    items: Vec<String>,
    version: u64,
    image_url: Option<String>,
    followed: bool,
}

impl TestPlaylist {
    fn snapshot(&self) -> String {
        format!("snapshot-{}", self.version)
    }

    fn advance(&mut self) -> String {
        self.version += 1;
        self.snapshot()
    }
}

struct SpotifyState {
    library: BTreeMap<&'static str, Vec<String>>,
    playlists: BTreeMap<String, TestPlaylist>,
    next_playlist: u64,
    current_uri: String,
    ordered_uris: Vec<String>,
    progress_ms: u64,
    is_playing: bool,
    shuffle: bool,
    repeat: String,
    active_device: String,
    volume_percent: u8,
    queue: Vec<String>,
}

impl Default for SpotifyState {
    fn default() -> Self {
        let library = BTreeMap::from([
            ("track", vec!["track-1".to_string()]),
            ("episode", vec!["episode-1".to_string()]),
            ("album", vec!["album-1".to_string()]),
            ("show", vec!["show-1".to_string()]),
            ("artist", vec!["artist-1".to_string()]),
        ]);
        let playlists = BTreeMap::from([(
            "playlist-1".to_string(),
            TestPlaylist {
                name: "Conformance Playlist".to_string(),
                items: vec![
                    "spotify:track:track-1".to_string(),
                    "spotify:track:track-2".to_string(),
                ],
                version: 1,
                image_url: None,
                followed: true,
            },
        )]);
        Self {
            library,
            playlists,
            next_playlist: 1,
            current_uri: "spotify:track:track-2".to_string(),
            ordered_uris: vec!["spotify:track:track-2".to_string()],
            progress_ms: 0,
            is_playing: false,
            shuffle: false,
            repeat: "off".to_string(),
            active_device: "device-1".to_string(),
            volume_percent: 50,
            queue: Vec::new(),
        }
    }
}

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

fn test_client(server: &MockServer) -> SpotifyClient {
    let token = StoredToken {
        access_token: "test-access".to_string(),
        refresh_token: "test-refresh".to_string(),
        expires_at: 4_000_000_000,
        scope: "user-library-read user-library-modify playlist-read-private playlist-modify-private user-read-playback-state user-modify-playback-state".to_string(),
        token_type: "Bearer".to_string(),
    };
    SpotifyClient::new(test_config())
        .expect("test client")
        .with_api_base_for_tests(format!("{}/v1", server.uri()))
        .with_token_cache(Arc::new(Mutex::new(Some(token))))
}

fn uri(kind: MediaKind, id: &str) -> ResourceUri {
    ResourceUri::spotify(kind, id).expect("canonical fixture URI")
}

fn uri_string(kind: MediaKind, id: &str) -> String {
    uri(kind, id).as_uri()
}

fn fixtures() -> ConformanceFixtures {
    let track_one = uri(MediaKind::Track, "track-1");
    let track_two = uri(MediaKind::Track, "track-2");
    let episode_one = uri(MediaKind::Episode, "episode-1");
    let episode_two = uri(MediaKind::Episode, "episode-2");
    let show_one = uri(MediaKind::Show, "show-1");
    let show_two = uri(MediaKind::Show, "show-2");
    let album_one = uri(MediaKind::Album, "album-1");
    let album_two = uri(MediaKind::Album, "album-2");
    let artist_one = uri(MediaKind::Artist, "artist-1");
    let artist_two = uri(MediaKind::Artist, "artist-2");
    let playlist = uri(MediaKind::Playlist, "playlist-1");
    let catalog_items = vec![
        track_one.clone(),
        episode_one.clone(),
        show_one.clone(),
        album_one.clone(),
        artist_one.clone(),
        playlist.clone(),
    ];
    ConformanceFixtures {
        search: catalog_items
            .iter()
            .cloned()
            .map(|expected_uri| SearchFixture {
                kind: expected_uri.kind(),
                query: "conformance".to_string(),
                expected_uri,
            })
            .collect(),
        catalog_items,
        recently_played: Some(track_two.clone()),
        library: vec![
            LibraryFixture {
                kind: MediaKind::Track,
                initially_saved: track_one.clone(),
                writable_unsaved: Some(track_two.clone()),
            },
            LibraryFixture {
                kind: MediaKind::Episode,
                initially_saved: episode_one,
                writable_unsaved: Some(episode_two),
            },
            LibraryFixture {
                kind: MediaKind::Album,
                initially_saved: album_one.clone(),
                writable_unsaved: Some(album_two),
            },
            LibraryFixture {
                kind: MediaKind::Show,
                initially_saved: show_one.clone(),
                writable_unsaved: Some(show_two),
            },
            LibraryFixture {
                kind: MediaKind::Artist,
                initially_saved: artist_one.clone(),
                writable_unsaved: Some(artist_two),
            },
        ],
        album: Some(album_one),
        artist: Some(artist_one),
        show: Some(show_one),
        playlist: Some(PlaylistFixture {
            uri: playlist,
            initial_items: vec![track_one.clone(), track_two.clone()],
        }),
        transport: Some(TransportFixture {
            primary: track_one,
            secondary: track_two,
            transfer_device_id: Some("device-2".to_string()),
            previous_progress_ms: 0,
        }),
    }
}

fn album(id: &str) -> Value {
    json!({
        "id": id,
        "uri": uri_string(MediaKind::Album, id),
        "name": format!("Album {id}"),
        "artists": [{"name": "Fixture Artist", "uri": "spotify:artist:artist-1"}],
        "images": [],
        "total_tracks": 2,
        "album_type": "album"
    })
}

fn track(id: &str) -> Value {
    json!({
        "type": "track",
        "id": id,
        "uri": uri_string(MediaKind::Track, id),
        "name": format!("Track {id}"),
        "duration_ms": 180_000,
        "explicit": false,
        "is_playable": true,
        "artists": [{"name": "Fixture Artist", "uri": "spotify:artist:artist-1"}],
        "album": album("album-1")
    })
}

fn episode(id: &str) -> Value {
    json!({
        "type": "episode",
        "id": id,
        "uri": uri_string(MediaKind::Episode, id),
        "name": format!("Episode {id}"),
        "duration_ms": 120_000,
        "show": {"name": "Fixture Show"},
        "images": []
    })
}

fn show(id: &str) -> Value {
    json!({
        "id": id,
        "uri": uri_string(MediaKind::Show, id),
        "name": format!("Show {id}"),
        "publisher": "Fixture Publisher",
        "images": [],
        "total_episodes": 2
    })
}

fn artist(id: &str) -> Value {
    json!({
        "id": id,
        "uri": uri_string(MediaKind::Artist, id),
        "name": format!("Artist {id}"),
        "images": [],
        "followers": {"total": 42}
    })
}

fn playlist(id: &str, value: &TestPlaylist) -> Value {
    let images = value
        .image_url
        .as_ref()
        .map(|url| vec![json!({"url": url})])
        .unwrap_or_default();
    json!({
        "id": id,
        "uri": uri_string(MediaKind::Playlist, id),
        "name": value.name,
        "owner": {"id": "fixture-owner", "display_name": "Fixture Owner"},
        "tracks": {"total": value.items.len()},
        "images": images,
        "snapshot_id": value.snapshot()
    })
}

fn playable(uri: &str) -> Value {
    let resource = ResourceUri::parse(uri).expect("fixture playable URI");
    match resource.kind() {
        MediaKind::Track => track(resource.bare_id()),
        MediaKind::Episode => episode(resource.bare_id()),
        _ => panic!("unsupported playable fixture: {uri}"),
    }
}

fn query(request: &Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
}

fn requested_page(request: &Request, items: Vec<Value>) -> Value {
    let total = items.len();
    let offset = query(request, "offset")
        .expect("paged request must include offset")
        .parse::<usize>()
        .expect("page offset must be numeric");
    let limit = query(request, "limit")
        .expect("paged request must include limit")
        .parse::<usize>()
        .expect("page limit must be numeric");
    let items = items
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    json!({"items": items, "total": total})
}

fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

fn empty_response() -> ResponseTemplate {
    ResponseTemplate::new(204)
}

fn library_key(path: &str) -> Option<&'static str> {
    match path {
        "/v1/me/tracks" => Some("track"),
        "/v1/me/episodes" => Some("episode"),
        "/v1/me/albums" => Some("album"),
        "/v1/me/shows" => Some("show"),
        "/v1/me/following" => Some("artist"),
        _ => None,
    }
}

fn library_item(kind: &str, id: &str) -> Value {
    match kind {
        "track" => json!({"track": track(id)}),
        "episode" => json!({"episode": episode(id)}),
        "album" => json!({"album": album(id)}),
        "show" => json!({"show": show(id)}),
        "artist" => artist(id),
        _ => panic!("unknown library kind: {kind}"),
    }
}

fn device(id: &str, active: bool, volume_percent: u8) -> Value {
    json!({
        "id": id,
        "name": format!("Device {id}"),
        "type": "Computer",
        "is_active": active,
        "is_restricted": false,
        "volume_percent": volume_percent,
        "supports_volume": true
    })
}

fn respond(request: &Request, shared: &StdMutex<SpotifyState>) -> ResponseTemplate {
    let method = request.method.as_str();
    let path = request.url.path();
    let mut state = shared.lock().expect("mock state lock");

    if method == "GET" && path == "/v1/search" {
        let kind = query(request, "type").expect("search type");
        let _query = query(request, "q").expect("search query");
        let (key, item) = match kind.as_str() {
            "track" => ("tracks", track("track-1")),
            "episode" => ("episodes", episode("episode-1")),
            "show" => ("shows", show("show-1")),
            "album" => ("albums", album("album-1")),
            "artist" => ("artists", artist("artist-1")),
            "playlist" => (
                "playlists",
                playlist(
                    "playlist-1",
                    state.playlists.get("playlist-1").expect("fixture playlist"),
                ),
            ),
            _ => return json_response(400, json!({"error": "unknown search type"})),
        };
        return json_response(200, json!({(key): requested_page(request, vec![item])}));
    }

    if method == "GET" && path == "/v1/tracks/track-1" {
        return json_response(200, track("track-1"));
    }
    if method == "GET" && path == "/v1/me/player/recently-played" {
        assert_eq!(query(request, "limit").as_deref(), Some("20"));
        return json_response(200, json!({"items": [{"track": track("track-2")}]}));
    }
    if method == "GET" && path == "/v1/albums/album-1/tracks" {
        return json_response(
            200,
            requested_page(request, vec![track("track-1"), track("track-2")]),
        );
    }
    if method == "GET" && path == "/v1/artists/artist-1/albums" {
        return json_response(
            200,
            requested_page(request, vec![album("album-1"), album("album-2")]),
        );
    }
    if method == "GET" && path == "/v1/shows/show-1/episodes" {
        return json_response(
            200,
            requested_page(request, vec![episode("episode-1"), episode("episode-2")]),
        );
    }

    if let Some(kind) = library_key(path) {
        if method == "GET" {
            let items = state
                .library
                .get(kind)
                .expect("library fixture")
                .iter()
                .map(|id| library_item(kind, id))
                .collect::<Vec<_>>();
            if kind == "artist" {
                assert_eq!(query(request, "type").as_deref(), Some("artist"));
                let limit = query(request, "limit")
                    .expect("followed-artists limit")
                    .parse::<usize>()
                    .expect("numeric followed-artists limit");
                let after = query(request, "after");
                let start = after
                    .as_deref()
                    .and_then(|cursor| {
                        state
                            .library
                            .get(kind)
                            .expect("artist fixture")
                            .iter()
                            .position(|id| id == cursor)
                    })
                    .map_or(0, |index| index + 1);
                let page_items = items
                    .into_iter()
                    .skip(start)
                    .take(limit)
                    .collect::<Vec<_>>();
                let consumed = page_items.len();
                let has_more =
                    start + consumed < state.library.get(kind).expect("artist fixture").len();
                let next_cursor = has_more
                    .then(|| {
                        page_items
                            .last()
                            .and_then(|item| item["id"].as_str())
                            .map(str::to_string)
                    })
                    .flatten();
                return json_response(
                    200,
                    json!({"artists": {
                        "items": page_items,
                        "next": next_cursor.as_ref().map(|cursor| format!(
                            "https://api.spotify.com/v1/me/following?after={cursor}"
                        )),
                        "cursors": {"after": next_cursor}
                    }}),
                );
            }
            return json_response(200, requested_page(request, items));
        }
        if method == "PUT" || method == "DELETE" {
            let ids = query(request, "ids").expect("library mutation ids");
            let saved = state.library.get_mut(kind).expect("library fixture");
            for id in ids.split(',') {
                if method == "PUT" && !saved.iter().any(|existing| existing == id) {
                    saved.push(id.to_string());
                } else if method == "DELETE" {
                    saved.retain(|existing| existing != id);
                }
            }
            return empty_response();
        }
    }

    if method == "GET" && path == "/v1/me/playlists" {
        let items = state
            .playlists
            .iter()
            .filter(|(_, value)| value.followed)
            .map(|(id, value)| playlist(id, value))
            .collect::<Vec<_>>();
        return json_response(200, requested_page(request, items));
    }
    if method == "POST" && path == "/v1/me/playlists" {
        let body = request.body_json::<Value>().expect("playlist create body");
        let id = format!("created-{}", state.next_playlist);
        state.next_playlist += 1;
        let value = TestPlaylist {
            name: body["name"].as_str().expect("playlist name").to_string(),
            items: Vec::new(),
            version: 1,
            image_url: None,
            followed: true,
        };
        let response = playlist(&id, &value);
        state.playlists.insert(id, value);
        return json_response(201, response);
    }

    if let Some(rest) = path.strip_prefix("/v1/playlists/") {
        if let Some(id) = rest.strip_suffix("/items") {
            if method == "GET" {
                let Some(value) = state.playlists.get(id).filter(|value| value.followed) else {
                    return ResponseTemplate::new(404);
                };
                let items = value
                    .items
                    .iter()
                    .map(|uri| json!({"track": playable(uri)}))
                    .collect::<Vec<_>>();
                return json_response(200, requested_page(request, items));
            }
            if method == "POST" {
                let body = request.body_json::<Value>().expect("playlist add body");
                let additions = body["uris"]
                    .as_array()
                    .expect("playlist add uris")
                    .iter()
                    .map(|uri| uri.as_str().expect("playlist URI").to_string())
                    .collect::<Vec<_>>();
                let position =
                    query(request, "position").and_then(|value| value.parse::<usize>().ok());
                let value = state.playlists.get_mut(id).expect("playlist add target");
                let position = position.unwrap_or(value.items.len()).min(value.items.len());
                value.items.splice(position..position, additions);
                let snapshot = value.advance();
                return json_response(201, json!({"snapshot_id": snapshot}));
            }
            if method == "PUT" {
                let body = request.body_json::<Value>().expect("playlist reorder body");
                let start = body["range_start"].as_u64().expect("range start") as usize;
                let length = body["range_length"].as_u64().expect("range length") as usize;
                let insert_before = body["insert_before"].as_u64().expect("insert before") as usize;
                let value = state
                    .playlists
                    .get_mut(id)
                    .expect("playlist reorder target");
                let moved = value
                    .items
                    .drain(start..start.saturating_add(length))
                    .collect::<Vec<_>>();
                let destination = if insert_before > start {
                    insert_before.saturating_sub(length)
                } else {
                    insert_before
                }
                .min(value.items.len());
                value.items.splice(destination..destination, moved);
                let snapshot = value.advance();
                return json_response(200, json!({"snapshot_id": snapshot}));
            }
            if method == "DELETE" {
                let body = request.body_json::<Value>().expect("playlist remove body");
                let mut positions = body["items"]
                    .as_array()
                    .expect("playlist remove items")
                    .iter()
                    .flat_map(|item| {
                        item["positions"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_u64)
                    })
                    .map(|position| position as usize)
                    .collect::<Vec<_>>();
                positions.sort_unstable_by(|left, right| right.cmp(left));
                let value = state.playlists.get_mut(id).expect("playlist remove target");
                for position in positions {
                    value.items.remove(position);
                }
                let snapshot = value.advance();
                return json_response(200, json!({"snapshot_id": snapshot}));
            }
        }
        if let Some(id) = rest.strip_suffix("/images") {
            if method == "PUT" {
                state
                    .playlists
                    .get_mut(id)
                    .expect("playlist image target")
                    .image_url = Some("https://images.test/cover.jpg".to_string());
                return empty_response();
            }
        }
        if let Some(id) = rest.strip_suffix("/followers") {
            if method == "DELETE" {
                state
                    .playlists
                    .get_mut(id)
                    .expect("playlist unfollow target")
                    .followed = false;
                return empty_response();
            }
        }
        if !rest.contains('/') && method == "GET" {
            let Some(value) = state.playlists.get(rest).filter(|value| value.followed) else {
                return ResponseTemplate::new(404);
            };
            return json_response(200, playlist(rest, value));
        }
    }

    if method == "GET" && path == "/v1/me/player" {
        let active = device(&state.active_device, true, state.volume_percent);
        return json_response(
            200,
            json!({
                "device": active,
                "repeat_state": state.repeat,
                "shuffle_state": state.shuffle,
                "progress_ms": state.progress_ms,
                "is_playing": state.is_playing,
                "item": playable(&state.current_uri)
            }),
        );
    }
    if method == "GET" && path == "/v1/me/player/devices" {
        return json_response(
            200,
            json!({
                "devices": [
                    device("device-1", state.active_device == "device-1", state.volume_percent),
                    device("device-2", state.active_device == "device-2", state.volume_percent)
                ]
            }),
        );
    }
    if method == "GET" && path == "/v1/me/player/queue" {
        let queue = state
            .queue
            .iter()
            .map(|uri| playable(uri))
            .collect::<Vec<_>>();
        return json_response(
            200,
            json!({"currently_playing": playable(&state.current_uri), "queue": queue}),
        );
    }
    if method == "POST" && path == "/v1/me/player/queue" {
        state.queue.push(query(request, "uri").expect("queued URI"));
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/play" {
        let body = request.body_json::<Value>().expect("play body");
        if let Some(uris) = body.get("uris").and_then(Value::as_array) {
            state.ordered_uris = uris
                .iter()
                .map(|uri| uri.as_str().expect("play URI").to_string())
                .collect();
            state.current_uri = state.ordered_uris[0].clone();
            state.progress_ms = body["position_ms"].as_u64().unwrap_or(0);
        }
        state.is_playing = true;
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/pause" {
        state.is_playing = false;
        return empty_response();
    }
    if method == "POST" && path == "/v1/me/player/next" {
        let next = state
            .ordered_uris
            .iter()
            .position(|uri| uri == &state.current_uri)
            .and_then(|index| state.ordered_uris.get(index + 1))
            .cloned();
        if let Some(next) = next {
            state.current_uri = next;
        }
        state.progress_ms = 0;
        return empty_response();
    }
    if method == "POST" && path == "/v1/me/player/previous" {
        state.progress_ms = 0;
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/seek" {
        state.progress_ms = query(request, "position_ms")
            .expect("seek position")
            .parse()
            .expect("numeric seek position");
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/volume" {
        state.volume_percent = query(request, "volume_percent")
            .expect("volume percent")
            .parse()
            .expect("numeric volume percent");
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/shuffle" {
        state.shuffle = query(request, "state").as_deref() == Some("true");
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player/repeat" {
        state.repeat = query(request, "state").expect("repeat state");
        return empty_response();
    }
    if method == "PUT" && path == "/v1/me/player" {
        let body = request.body_json::<Value>().expect("transfer body");
        state.active_device = body["device_ids"][0]
            .as_str()
            .expect("transfer device")
            .to_string();
        state.is_playing = body["play"].as_bool().unwrap_or(false);
        return empty_response();
    }

    json_response(
        500,
        json!({"error": format!("unhandled conformance request: {method} {path}")}),
    )
}

#[tokio::test]
async fn spotify_adapter_passes_provider_and_transport_conformance() {
    let server = MockServer::start().await;
    let state = Arc::new(StdMutex::new(SpotifyState::default()));
    let responder_state = Arc::clone(&state);
    Mock::given(any())
        .respond_with(move |request: &Request| respond(request, &responder_state))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let fixtures = fixtures();
    run_provider_conformance(&client, &fixtures, ConformanceOptions::default())
        .await
        .expect("Spotify provider conformance");
    let transport_caps = client
        .capabilities()
        .transport
        .expect("Spotify transport capabilities");
    run_transport_conformance(&client, &transport_caps, &fixtures)
        .await
        .expect("Spotify transport conformance");
}
