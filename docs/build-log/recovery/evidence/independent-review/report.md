## Independent review report — spotuify recovery candidate

### Identity and exact references

- Base / merge base: `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e` (as supplied by the brief)
- Exact candidate: `9853765585fb4370356c4036d047d29c75f0eb30`
- Tree: `cabf1e45d4d608fe1e89598aab71ddbaba1ea13c`
- Model/provider: **not supplied by this runtime**. I did not invent a self-identity. The brief's reasoning paragraph describes a run-owned Pi request hook on an audited DeepSeek-v4-pro cloud route; I have no runtime metadata of my own with which to confirm or contradict that, so I make no claim about it.
- Reviewer posture: read-only; `read`/`grep`/`find`/`ls` only; no commands executed, no tests re-run, no builds, no live Spotify or device behavior observed.

### Evidence inspected

I reviewed the attached whole-diff patch and verified every code hunk against the snapshot tree, reading full surrounding regions:

- **Handoff/Activated relay** — `crates/spotuify-daemon/src/state.rs` (guard struct 749–791, actor `Reconnect` branch ~4880–4955, `reconnect_player` 2595–2615, health/stall recovery paths 3600–3680 and 3755–3810, `forward_player_events` 5482–5720, `resume_target_after_drop` 5820–5843, `schedule_player_reconnect` 5845–5893, `translate_player_event` 5910–5950; all five named handoff regressions present at 7975/8232/8273/8332/8458, including the tracker-mutex-held registration race).
- **Clock/session** — `clock.rs` Deactivated freeze arm (~220–240), deterministic `current_progress_ms(at)` tests (725–780); `session_tracker.rs` Deactivated→Idle/SessionDied (306–320).
- **Player side** — `events.rs` variants; `embedded/mod.rs` event task (660–690: deactivation clears the activation latch, SessionConnected sets it; true loss still comes from the Spirc task wrapper 715–730) and `translate_librespot_player_event` mapping; `mock.rs` (Activated before play/resume, registration gate, Ready barrier); `tests/mock_backend.rs` 8-event order.
- **CLI** — `commands.rs` (`ipc_play_uri_with_context` 1848–1937, `require_queue_resource` 317–337, `canonical_resource` 403–429, `ipc_queue_add` 3413–3523, `daemon_playlist_target` 3816–3843, `ipc_playlist` Tracks/Play arms 2330–2365).
- **MCP** — `bridge.rs` play arm and `play_context_uri` 737–800; `rpc.rs` schema 644–684 and `required_props_for` 704+; `tools.rs` pre-dispatch validation 648–670; tests in `tests/rpc.rs` 1386+ and `tests/tool_routing.rs` 206+.
- **TUI** — `app.rs` (`requests_for_action` PlaySelected 2339–2363, `visible_items` 2036+, `selected_count` 2285+, `selected_playlist_target` 2479+, `navigate_search_results` 2601–2630 and movement functions, `action_from_key` 6260–6390, `activate_selected` 7277–7330, `next_index`/`prev_index` 8915–8929; mouse-target test 11810–11840); `ui.rs` (`render_search` 3559–3629, `render_search_groups` 3631–3754, `register_row_hits`, `render_media_rows` budgets, `truncate_media_row` 3908–3925, hint bar 5187–5232, hit-map clear at 72; CJK/width tests 7362+).
- **Diagnostics/errors** — `diagnostics.rs` (310–320 append-refresh, `finalize_report` 460–464, `refresh_health_class` 466–500); `handlers/admin.rs` doctor rollup 320–360; `handler.rs` `error_response_from` 1404–1520 and receipt provider fill at 6928–6935; `handlers/playback.rs` restricted check and 404 hint 519–625; `spotuify_protocol::redact_sensitive_text` 2593+ (label + entropy passes leave "HTTP 404"/endpoint intact); `logging.rs` bounded tail 115–180 with all seven tests; `src/main.rs` doctor 3051–3077 and `spotuify-launcher::daemon_status` (366–405 — never auto-starts a daemon, so the offline local fallback is genuine).
- **Provider** — `spotuify-spotify/provider.rs` 403 classification (private keyword only → Private; otherwise typed Forbidden naming the playlist URI) plus its two fixtures.
- **CI/signing** — `ci.yml` (macos-smoke routing expression, hosted PRs, scope patterns, release-scripts job), `release.yml` (jq matrix rewrite, `MATRIX_TARGET` present in the signing step env, `timeout-minutes: 60`, rust-cache skip on self-hosted), `self-hosted-rust-home/action.yml`, `scripts/ci-cargo` (inner caps override the wrapper's five), `cargo-test`/`cargo-nextest` (pgrep sweep removal), `cargo_runner_test.sh`, `ci_cargo_test.sh`, `sign_macos_cli.sh` + stub test, `release_change_scope.sh` + test, `.cargo/config.toml`.
- **Site/docs** — `site/package.json` (Astro 7.3.3 exact pin, sharp range unchanged), lock churn, generated reference and `play-uri.md`; conflict-marker scan over the whole tree returned zero matches; decision-log tail D-042–D-054 cleanly appended.
- **Integration** — `tests/cli_fake_daemon.rs` journey test 394–494; committed evidence folder contents (fake journey JSON, captures, site-audit artifacts, runner evidence).

### In-scope blockers

**None found.** The candidate implements the agreed scope and I found no concrete contradiction of the user promise, no data-loss/duplication path, and no introduced security/privacy leak. Specifically, the concurrency-critical delta is correct as coded:

- Deactivation revokes recovery synchronously before any await (`state.rs:5490-5496` — generation bump + active-flag clear precede the `session_tracker.observe` await), closing the stale-playing fallback the root fixed and the registration race proves.
- Stale loader events (`TrackChanged`/`Playing`/ticks/pauses/EoT) between Deactivated and Activated are gated by `inactive_after_handoff`; Deactivated itself still reaches the tracker/clock/viz so local truth is updated; foreign-device volume and foreign playback snapshots survive (volume cache retained, clock apply gated by `current_device_is_own`); activation alone never declares playing or sets `we_are_active`.
- All three recovery scheduling paths carry the generation/ownership guard; explicit reconnect bumps the generation and carries `recovery: None`; the actor re-checks before teardown, after teardown, and after registration; a superseded queued recovery leaves the connected player untouched.
- True connection loss retains its path via the Spirc task wrapper's domain `SessionDisconnected`.
- CLI context playback validates kinds/provider/scheme before mutation and preserves the Liked Songs sentinel; explicit playlist URIs/share links reach the provider without saved-list lookup while names still resolve through saved playlists; queue collection reads are capability-gated; MCP validates the same contract before daemon dispatch; TUI Enter plays the selected filtered track in its playlist context and `e` remains queue with the accurate playlist hint.

### Nonblocking notes (root triage; no correction required to merge)

1. **`scripts/sign_macos_cli.sh:24`** — `[[ -f "$KEYCHAIN" ]]` never matches a real keychain (macOS keychains are directories), so `security delete-keychain` is dead code in production. No credential leak results: `rm -rf "$signing_dir"` (line 27) removes the keychain directory and all temporary secrets, and the search list is restored on EXIT. The stub test passes only because its fake `create-keychain` touches a file. Smallest correction if desired: test `-d`. Related edge: a host with zero user keychains would fail the `-s` restore with empty args — unlikely on CI runners.
2. **MCP share-link asymmetry** — `play_context_uri` validates via `ResourceUri::parse` (`spotuify-core/src/uri.rs:166`), which accepts only strict `scheme:kind:id`. So share links are accepted for CLI `play-uri --context` / `playlist tracks` but rejected by MCP pre-validation when `context_uri` is present. The MCP schema explicitly advertises "URI from the same provider" and pre-dispatch validation is deliberate, so this does not contradict the parity criterion (kind/provider validation is identical); it is a documented surface difference, not a blocker.
3. **Unverifiable external gates** — I did not run the workspace tests, Clippy, native build/smoke, npm audit/build, or any live journey. The brief itself marks those gates as pending; the committed logs/JSON are evidence artifacts I inspected but did not re-execute. No claim about physical audio, runner execution, or Spotify 403/404 causes is made here.
4. Cosmetic: liked-songs Enter now toasts "Playing {name}" rather than the prior "… from Liked Songs" wording; hint text differs slightly at small widths (`S sort … / T type …`). Human-aesthetic review (S4) remains separate as disclosed.

### Verification limits

Read-only file inspection against the supplied base/head/tree and diff; no git operations to re-derive hashes, no test/build/command execution, no network or device checks. Findings are confined to source-verifiable contradictions; anything requiring runtime behavior (tests, lint, native CI, site audit, live playlist/transfer/handoff outcomes) is explicitly outside what I can certify here.
