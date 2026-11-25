# The playlist auth fix is released and installed in 0.1.116

## The live playlist contains all 80 approved tracks

The installed `spotuify` 0.1.112 sent playlist creation through the first-party
bearer and received HTTP 429 twice. Both attempts occurred on November 24 UTC.
The developer-app probe returned HTTP 201 for `POST /me/playlists` and
`POST /playlists/{id}/items`. A fresh API read confirmed all 80 URIs in order.
The installed CLI then returned the same ordered list through
`spotuify playlist tracks <uri> --format json`.

The probe read the existing developer-app access token into process memory.
It changed no stored credentials and printed no tokens.

## The local fix changes only creation and additions

Implementation branch: `fix/playlist-dev-app-auth`.
Initial base: freshly fetched `origin/main` at
`5c88594b619bcb9e2b7c8dc64c741dd0fcc26b00`.
The original checkout contains pre-existing changes and remains untouched.

`crates/spotuify-spotify/src/client.rs` keeps current playlist creation and
item-addition POSTs on the primary bearer. Its existing route selector also
controls refresh and cooldown keys. First-party-only setups keep their primary
first-party bearer. Other writes retain their previous routes.

The regression in `crates/spotuify-spotify/tests/playlist_responses.rs` uses a
throttled first-party credential and a working developer-app credential.
It requires actual create and add HTTP requests with the developer-app bearer,
the approved visibility field, and ordered track URIs. It failed before the fix
with `RateLimited { retry_after: 60s, scope: "first-party" }` and passed after it.

[D-055](../blueprint/13-decision-log.md#d-055-playlist-creation-and-item-additions-use-the-developer-app-bearer-2026-11-25)
records the scope. The auth blueprint describes the current routes.

## Verification

Run heavy commands through `agent-work run --` and keep this worktree's
`CARGO_TARGET_DIR` separate.

```sh
cargo test --locked -p spotuify-spotify
cargo fmt --all -- --check
git diff --check
```

All 233 adapter tests passed, including the regression, first-party-only routing,
and bearer-specific cooldown checks. Formatting and whitespace checks passed.
The CLI build without default features, fake-provider CLI smoke, and adapter
Clippy check passed. The smoke script stopped its isolated daemon.

Full workspace CI passed at `3c054e46d82fe39a7a9fe628344a437c037a0242`:
https://github.com/planetaryescape/spotuify/actions/runs/45512390997.
The local serial workspace run passed 2,038 tests, including doctests.
The first local run hit two existing 25 ms sync-test deadlines under parallel
execution. That test file passed serially before the full serial rerun.
Rust advisories and licenses passed. The website audit reported zero vulnerabilities,
and the website build passed. Its local install used `npm ci --ignore-scripts`
because the machine's install-script policy rejected optional `fsevents` hooks.

## Release 0.1.116 is installed

The live playlist is complete. BK authorized the full release and approved
reviewing and including dependency repair PR #190 to clear existing audit failures.
The reviewed head was `819f1310cbea6f68c7631278026f424116189ea7`.
Its GitHub Actions checks passed. The vendored driver matches the published
SQLx 0.8.6 crate except for the SQLite dependency and three whitespace edits.

PR #193 merged by rebase. The fix is `585f518b5d04420cfce2511b413674b90dd0f8af`;
its documentation is `3e2ede29a8badedb4f53a1c27b87101532118221`.
Release PR #186 merged separately as 0.1.115 before this fix landed.
Do not change that tag. Release-please opened
[PR #194 for 0.1.116](https://github.com/planetaryescape/spotuify/pull/194).

PR #194 passed its checks and merged by squash. Release commit and tag
`v0.1.116` are `2703b443c8d4a4f306be0edc6831801b31add97b`.
[Release Please](https://github.com/planetaryescape/spotuify/actions/runs/45516550568)
created the tag and release.
The [artifact workflow](https://github.com/planetaryescape/spotuify/actions/runs/45516591296)
passed all four platform builds, release smoke, publication, and Homebrew verification.
The [release](https://github.com/planetaryescape/spotuify/releases/tag/v0.1.116)
contains four archives and their SHA-256 files. Its notes retain the reviewed
playlist fix text.

The downloaded Apple Silicon archive passed its published checksum:
`d1d2cd66d8be13ef7a6b3d6254b9f18d2bd19482f0937f6a87024c2de7f20b66`.
A throwaway-root Cargo install from that tag built successfully and printed
`spotuify 0.1.116`. The throwaway install was removed afterward.

Homebrew upgraded from 0.1.112 to 0.1.116. The old daemon exited before the new
Homebrew daemon started. Both the CLI and daemon report 0.1.116.
The only executable on PATH is `/opt/homebrew/bin/spotuify`.
Both credential types remain present, with no missing scopes or required login.
The upgraded CLI returned all 80 approved playlist URIs in exact order.
No playback command was sent.

The downloadable skill passed its freshness check and matches the local skill.
The final website build contains the 0.1.116 heading and reviewed notes.
The public changelog remains unavailable: on November 25 at 00:54 UTC,
`https://spotuify.app/changelog/` returned HTTP 402 with
`x-vercel-error: DEPLOYMENT_DISABLED`. No hosting or billing setting changed.

The CLI release is complete. Restoring the public website is separate work.

One separate defect remains: `spotuify auth bearer --reveal-secret` returns a
session-backed first-party token even when auth status prefers developer-app.
The probe using that command received HTTP 429 on its first read. Reading the
stored developer-app token produced HTTP 200 for the same read, then successful
creation and population. `crates/spotuify-daemon/src/state.rs::web_api_bearer`
constructs a `FirstPartyBearerProvider`. Do not use the command as proof that a
probe uses developer-app auth. Repairing that command is outside this routing fix.
