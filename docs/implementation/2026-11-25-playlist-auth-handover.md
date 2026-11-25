# Developer-app auth created and populated the playlist

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

Branch: `fix/playlist-dev-app-auth`.
Base: freshly fetched `origin/main` at
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

## Handover

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

Next: finish #194's release notes and website verification, merge it by squash,
then verify the tag, release assets, Homebrew update, and installed daemon.
The release branch includes release-please's version and synchronized lockfile.
No local installation changed yet.

One separate defect remains: `spotuify auth bearer --reveal-secret` returns a
session-backed first-party token even when auth status prefers developer-app.
The probe using that command received HTTP 429 on its first read. Reading the
stored developer-app token produced HTTP 200 for the same read, then successful
creation and population. `crates/spotuify-daemon/src/state.rs::web_api_bearer`
constructs a `FirstPartyBearerProvider`. Do not use the command as proof that a
probe uses developer-app auth. Repairing that command is outside this routing fix.
