# Fix: Album save/unsave via macOS app and CLI

## What was broken

Saving an album to library via the macOS app's "Save to Library" button, `spotuify like <album-uri>`, or `spotuify save <album-uri>` silently failed.

## Root causes

Two independent bugs, both introduced in commit `f79917f`.

### 1. Wrong Spotify endpoint

`library_endpoint_for_uri` in `crates/spotuify-spotify/src/client.rs` routed all library saves through `/me/library?uris=<uri>`. That endpoint does not exist in the Spotify Web API.

Correct endpoints:
- Tracks: `PUT /me/tracks?ids=<id>`
- Albums: `PUT /me/albums?ids=<id>`
- Episodes: `PUT /me/episodes?ids=<id>`
- Shows: `PUT /me/shows?ids=<id>`
- Artists: `PUT /me/following?type=artist&ids=<id>`

### 2. `LibrarySave` routed through `save_item`, which rejects albums

`crates/spotuify-daemon/src/handlers/library.rs` built a `CommandKind::SaveItem` and called `actions::execute`. `save_item` explicitly rejects non-track, non-episode, and non-show URIs.

The fix calls `client.library_save_by_uri(&uri)` directly.

## Files changed

- `crates/spotuify-spotify/src/client.rs`
- `crates/spotuify-spotify/src/endpoints.rs`
- `crates/spotuify-spotify/tests/client_empty_body.rs`
- `crates/spotuify-daemon/src/handlers/library.rs`
