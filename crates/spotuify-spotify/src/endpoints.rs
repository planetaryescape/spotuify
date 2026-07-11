//! Single source of truth for every Spotify Web API path spotuify hits.
//!
//! When Spotify migrates an endpoint (e.g. the 2024 move of
//! `POST /playlists/{id}/tracks` → `POST /playlists/{id}/items`, or the
//! deprecation of `POST /users/{user_id}/playlists` in favour of
//! `POST /me/playlists`), changing the path here makes every caller
//! follow automatically. Without this module we had path strings
//! scattered across `client.rs`, and we drifted onto two deprecated
//! endpoints silently — both surfaced as `403`s that triggered weeks of
//! misdirected auth work.
//!
//! Static paths are `pub const`. Paths that take a Spotify id are
//! `pub fn` builders that URL-encode the id internally (via
//! [`crate::client::encode_component`]) so callers can pass raw ids.
//!
//! Verified against <https://developer.spotify.com/documentation/web-api>
//! as of 2026-05-26.

use crate::client::encode_component;

// ---- User profile ----
pub const ME: &str = "/me";

// ---- Playback (`/me/player`) ----
pub const PLAYBACK: &str = "/me/player";
pub const DEVICES: &str = "/me/player/devices";
pub const QUEUE: &str = "/me/player/queue";
pub const RECENTLY_PLAYED: &str = "/me/player/recently-played";
pub const PLAY: &str = "/me/player/play";
pub const PAUSE: &str = "/me/player/pause";
pub const NEXT: &str = "/me/player/next";
pub const PREVIOUS: &str = "/me/player/previous";
pub const SEEK: &str = "/me/player/seek";
pub const REPEAT: &str = "/me/player/repeat";
pub const SHUFFLE: &str = "/me/player/shuffle";
pub const VOLUME: &str = "/me/player/volume";

// ---- Library reads (per-type, current) ----
pub const SAVED_TRACKS: &str = "/me/tracks";
pub const SAVED_ALBUMS: &str = "/me/albums";
pub const SAVED_EPISODES: &str = "/me/episodes";
pub const SAVED_SHOWS: &str = "/me/shows";

/// Follow/unfollow + "is following" for artists and users.
pub const FOLLOWING: &str = "/me/following";

// ---- Playlists ----
/// Both list-my-playlists (GET) and create-playlist (POST).
pub const MY_PLAYLISTS: &str = "/me/playlists";

pub fn playlist(id: &str) -> String {
    format!("/playlists/{}", encode_component(id))
}

/// Modern playlist-items endpoint. Replaces the deprecated
/// `/playlists/{id}/tracks` for GET/POST/PUT/DELETE.
pub fn playlist_items(id: &str) -> String {
    format!("/playlists/{}/items", encode_component(id))
}

pub fn playlist_followers(id: &str) -> String {
    format!("/playlists/{}/followers", encode_component(id))
}

/// Custom cover-art upload. `PUT` accepts base64-encoded JPEG as a
/// raw text body with `Content-Type: image/jpeg`; max 256 KB. Needs
/// the `ugc-image-upload` scope.
pub fn playlist_image(id: &str) -> String {
    format!("/playlists/{}/images", encode_component(id))
}

// ---- Catalog ----
pub const SEARCH: &str = "/search";
/// Batch track lookup. Caller adds `?ids=...`.
pub const TRACKS_LOOKUP: &str = "/tracks";
/// Batch artist lookup. Caller adds `?ids=...`.
pub const ARTISTS_LOOKUP: &str = "/artists";

pub fn track(id: &str) -> String {
    format!("/tracks/{}", encode_component(id))
}

pub fn album_tracks(album_id: &str) -> String {
    format!("/albums/{}/tracks", encode_component(album_id))
}

pub fn artist_albums(artist_id: &str) -> String {
    format!("/artists/{}/albums", encode_component(artist_id))
}

pub fn show_episodes(show_id: &str) -> String {
    format!("/shows/{}/episodes", encode_component(show_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_items_uses_modern_path_not_deprecated_tracks() {
        // Locks in the migration: writes to a playlist must go to
        // `/playlists/{id}/items`, not the deprecated `/tracks` form
        // that Spotify 403s on dev-mode apps.
        let path = playlist_items("abc123");
        assert_eq!(path, "/playlists/abc123/items");
        assert!(!path.contains("/tracks"));
    }

    #[test]
    fn create_playlist_uses_me_not_users_user_id() {
        // The `/users/{user_id}/playlists` form appears to require
        // Extended Quota Mode; `/me/playlists` works for any user with
        // `playlist-modify-public`/`playlist-modify-private`.
        assert_eq!(MY_PLAYLISTS, "/me/playlists");
        assert!(!MY_PLAYLISTS.contains("/users/"));
    }

    #[test]
    fn id_builders_url_encode_special_characters() {
        // Spotify ids are normally `[A-Za-z0-9]` but URIs can carry
        // slashes/colons that MUST be encoded so we don't accidentally
        // open a path-traversal-shaped request.
        assert_eq!(playlist("a/b"), "/playlists/a%2Fb");
        assert_eq!(track("with:colon"), "/tracks/with%3Acolon");
    }
}
