use std::collections::BTreeMap;

use spotuify_core::{
    ArtistRef, ItemSource, MediaItem, MediaKind, Playlist, ProviderId, ResourceUri, UriScheme,
};

pub(crate) struct Fixtures {
    pub media: BTreeMap<String, MediaItem>,
    pub relations: BTreeMap<String, Vec<String>>,
    pub playlists: Vec<(Playlist, Vec<String>)>,
    pub library: Vec<String>,
    pub followed: Vec<String>,
    pub recent: Vec<String>,
}

pub(crate) fn standard(provider_id: &ProviderId, scheme: &UriScheme) -> Fixtures {
    let artist_uri = uri(scheme, MediaKind::Artist, "artist-1");
    let artist_two_uri = uri(scheme, MediaKind::Artist, "artist-2");
    let album_uri = uri(scheme, MediaKind::Album, "album-1");
    let show_uri = uri(scheme, MediaKind::Show, "show-1");
    let track_one_uri = uri(scheme, MediaKind::Track, "track-1");
    let track_two_uri = uri(scheme, MediaKind::Track, "track-2");
    let episode_uri = uri(scheme, MediaKind::Episode, "episode-1");
    let playlist_uri = uri(scheme, MediaKind::Playlist, "playlist-1");

    let mut media = BTreeMap::new();
    media.insert(
        artist_uri.clone(),
        item(
            &artist_uri,
            MediaKind::Artist,
            "Fake Artist",
            "Provider fixture",
            provider_id,
        ),
    );
    media.insert(
        artist_two_uri.clone(),
        item(
            &artist_two_uri,
            MediaKind::Artist,
            "Fake Artist Two",
            "Provider fixture",
            provider_id,
        ),
    );
    media.insert(
        album_uri.clone(),
        MediaItem {
            artists: vec![ArtistRef {
                name: "Fake Artist".to_string(),
                uri: artist_uri.clone(),
            }],
            ..item(
                &album_uri,
                MediaKind::Album,
                "Fake Album",
                "Fake Artist",
                provider_id,
            )
        },
    );
    media.insert(
        show_uri.clone(),
        item(
            &show_uri,
            MediaKind::Show,
            "Fake Show",
            "Provider fixture",
            provider_id,
        ),
    );
    for (uri, name) in [
        (&track_one_uri, "Fake Track One"),
        (&track_two_uri, "Fake Track Two"),
    ] {
        media.insert(
            uri.to_string(),
            MediaItem {
                album: Some("Fake Album".to_string()),
                album_uri: Some(album_uri.clone()),
                artists: vec![ArtistRef {
                    name: "Fake Artist".to_string(),
                    uri: artist_uri.clone(),
                }],
                ..item(uri, MediaKind::Track, name, "Fake Artist", provider_id)
            },
        );
    }
    media.insert(
        episode_uri.clone(),
        item(
            &episode_uri,
            MediaKind::Episode,
            "Fake Episode",
            "Fake Show",
            provider_id,
        ),
    );
    media.insert(
        playlist_uri.clone(),
        item(
            &playlist_uri,
            MediaKind::Playlist,
            "Fake Favorites",
            "fake-user",
            provider_id,
        ),
    );

    let mut relations = BTreeMap::new();
    relations.insert(
        album_uri.clone(),
        vec![track_one_uri.clone(), track_two_uri.clone()],
    );
    relations.insert(artist_uri.clone(), vec![album_uri]);
    relations.insert(show_uri, vec![episode_uri]);

    Fixtures {
        media,
        relations,
        playlists: vec![(
            Playlist {
                id: playlist_uri,
                name: "Fake Favorites".to_string(),
                owner: "fake-user".to_string(),
                tracks_total: 2,
                image_url: None,
                version_token: Some("v1".to_string()),
            },
            vec![track_one_uri.clone(), track_two_uri.clone()],
        )],
        library: vec![track_one_uri.clone()],
        followed: vec![artist_uri],
        recent: vec![track_two_uri, track_one_uri],
    }
}

/// Preserve the long-standing CLI/smoke fixture behind
/// `SPOTUIFY_FAKE_SPOTIFY` while keeping it out of the real Spotify adapter.
pub(crate) fn spotify_compatibility(provider_id: &ProviderId, scheme: &UriScheme) -> Fixtures {
    let luther_uri = uri(scheme, MediaKind::Artist, "luther-vandross");
    let chaka_uri = uri(scheme, MediaKind::Artist, "chaka-khan");
    let album_uri = uri(scheme, MediaKind::Album, "never-too-much-album");
    let second_album_uri = uri(scheme, MediaKind::Album, "rufusized");
    let first_uri = uri(scheme, MediaKind::Track, "never-too-much");
    let second_uri = uri(scheme, MediaKind::Track, "sweet-thing");
    let playlist_uri = uri(scheme, MediaKind::Playlist, "quiet-storm");

    let mut media = BTreeMap::new();
    media.insert(
        luther_uri.clone(),
        item(
            &luther_uri,
            MediaKind::Artist,
            "Luther Vandross",
            "Artist",
            provider_id,
        ),
    );
    media.insert(
        chaka_uri.clone(),
        item(
            &chaka_uri,
            MediaKind::Artist,
            "Chaka Khan",
            "Artist",
            provider_id,
        ),
    );
    media.insert(
        album_uri.clone(),
        MediaItem {
            image_url: Some("https://picsum.photos/seed/never-too-much-album/640".to_string()),
            artists: vec![ArtistRef {
                name: "Luther Vandross".to_string(),
                uri: luther_uri.clone(),
            }],
            ..item(
                &album_uri,
                MediaKind::Album,
                "Never Too Much",
                "Luther Vandross",
                provider_id,
            )
        },
    );
    media.insert(
        second_album_uri.clone(),
        MediaItem {
            artists: vec![ArtistRef {
                name: "Chaka Khan".to_string(),
                uri: chaka_uri.clone(),
            }],
            ..item(
                &second_album_uri,
                MediaKind::Album,
                "Rufusized",
                "Rufus featuring Chaka Khan",
                provider_id,
            )
        },
    );
    media.insert(
        first_uri.clone(),
        MediaItem {
            album: Some("Never Too Much".to_string()),
            album_uri: Some(album_uri.clone()),
            context: "Never Too Much".to_string(),
            artists: vec![ArtistRef {
                name: "Luther Vandross".to_string(),
                uri: luther_uri.clone(),
            }],
            duration_ms: 221_000,
            image_url: Some("https://picsum.photos/seed/never-too-much/640".to_string()),
            genre: Some("R&B/Soul".to_string()),
            ..item(
                &first_uri,
                MediaKind::Track,
                "Never Too Much",
                "Luther Vandross",
                provider_id,
            )
        },
    );
    media.insert(
        second_uri.clone(),
        MediaItem {
            album: Some("Rufusized".to_string()),
            album_uri: Some(second_album_uri.clone()),
            context: "Rufus featuring Chaka Khan".to_string(),
            artists: vec![ArtistRef {
                name: "Chaka Khan".to_string(),
                uri: chaka_uri.clone(),
            }],
            duration_ms: 199_000,
            image_url: Some("https://picsum.photos/seed/sweet-thing/640".to_string()),
            genre: Some("Funk".to_string()),
            ..item(
                &second_uri,
                MediaKind::Track,
                "Sweet Thing",
                "Chaka Khan",
                provider_id,
            )
        },
    );
    media.insert(
        playlist_uri.clone(),
        item(
            &playlist_uri,
            MediaKind::Playlist,
            "Quiet Storm",
            "Fake User",
            provider_id,
        ),
    );

    let mut relations = BTreeMap::new();
    relations.insert(album_uri, vec![first_uri.clone()]);
    relations.insert(second_album_uri, vec![second_uri.clone()]);
    relations.insert(luther_uri.clone(), Vec::new());
    relations.insert(chaka_uri, Vec::new());

    Fixtures {
        media,
        relations,
        playlists: vec![(
            Playlist {
                id: playlist_uri,
                name: "Quiet Storm".to_string(),
                owner: "Fake User".to_string(),
                tracks_total: 2,
                image_url: None,
                version_token: Some("fake-snap-1".to_string()),
            },
            vec![first_uri.clone(), second_uri.clone()],
        )],
        library: vec![first_uri.clone()],
        followed: vec![luther_uri],
        recent: vec![second_uri, first_uri],
    }
}

pub(crate) fn uri(scheme: &UriScheme, kind: MediaKind, id: &str) -> String {
    ResourceUri::new(scheme.clone(), kind, id)
        .expect("fake fixture URI must be canonical")
        .as_uri()
}

fn item(
    uri: &str,
    kind: MediaKind,
    name: &str,
    subtitle: &str,
    provider_id: &ProviderId,
) -> MediaItem {
    let parsed = ResourceUri::parse(uri).expect("fake fixture URI must parse");
    MediaItem {
        id: Some(parsed.bare_id().to_string()),
        uri: uri.to_string(),
        name: name.to_string(),
        subtitle: subtitle.to_string(),
        context: "Fake Provider".to_string(),
        duration_ms: 180_000,
        kind,
        source: Some(ItemSource::Provider(provider_id.to_string())),
        is_playable: Some(true),
        ..Default::default()
    }
}
