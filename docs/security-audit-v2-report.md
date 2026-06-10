# Spotuify Public-Release Security Audit Report (v2)

> Independent second-pass audit conducted against [`security-audit-rubric-v2.md`](./security-audit-rubric-v2.md). Performed 2026-05-27 on commit `98f707c` (`release: prepare spotuify 0.1.24`).

## Current status on 2026-06-09

This report is historical evidence, not the current bug list. The original
Medium findings have been fixed on `main`:

| Finding | Current status | Code truth |
|---|---|---|
| M1 - floating GitHub Action tags | Resolved | `.github/workflows/*.yml` pins third-party actions to commit SHAs with tag comments. |
| M2 - missing license gate | Resolved | `deny.toml` has a `[licenses]` allowlist, and CI runs `cargo deny check advisories licenses`. |
| M3 - MCP HTTP origin/host hardening | Resolved by Host validation | `crates/spotuify-mcp/src/http.rs` requires a loopback `Host`; `Origin` is still optional for non-browser clients. |
| L2 - log directory mode | Resolved | `crates/spotuify-daemon/src/logging.rs` sets the log directory to `0700` on Unix. |
| L5 - site dependency lags | Resolved | `site/package.json` is on the audited target versions: Starlight `0.39.2`, Astro `6.3.6`, Sharp `0.34.5`. |

Still valid: macOS release archives are unsigned/not notarized, and the one
accepted RustSec advisory remains documented in `deny.toml`.

Auth storage changed after the original audit: current builds store Spotify
credentials under the private config auth directory (`<config_dir>/auth/`) with
directory mode `0700` and file mode `0600` on Unix. This replaces the old
Keychain-backed path and removes OS credential prompts.

Update-awareness also changed after the original audit. Current builds contact
the public, unauthenticated GitHub releases API on daemon startup and then about
every six hours so CLI, TUI, and macOS app clients can show upgrade hints. This
is not a binary self-updater: it downloads no replacement artifact and can be
disabled with `SPOTUIFY_NO_UPDATE_CHECK=1`.

## Original verdict: **Ship now, with three fixable Medium follow-ups**

No Critical or High findings. The binary, daemon, and docs site behave the way a Spotify controller should: tokens stay in private auth files with restrictive permissions, network egress was limited to Spotify endpoints (+ optional lyrics) and library `lrclib.net` at audit time, the daemon IPC stays local (Unix sockets on Unix, named pipes on Windows), the MCP HTTP bridge is loopback + bearer-gated, and there is no telemetry, no binary self-updater, no silent autostart. Current builds additionally perform the bounded GitHub release check described above.

The Medium items below are defence-in-depth gaps that an attentive auditor (Homebrew core reviewer, F-Droid maintainer, a security-conscious user) might raise. None of them blocks a public release. None of them would credibly cause a malware classification.

## What was checked

| Category | Method |
|---|---|
| **SEC-A1** Credentials & secrets | Subagent + targeted reads of `auth.rs`, credential storage code, config writer, `bug-report` redaction. |
| **SEC-A2** Network & process | Subagent enumeration of `reqwest`/`Command::new` call sites, install scripts, autostart units. |
| **SEC-A3** IPC trust boundaries | Subagent + direct reads of socket-path resolution, frame codec, MCP HTTP middleware, confirm-gate. |
| **SEC-A4** Distribution trust | Subagent of `.github/workflows/*.yml`, `packaging/homebrew/`, `install/`, install instructions. |
| **SEC-A5** Supply chain | `cargo deny check advisories` (clean), `cargo deny check bans/sources` (clean), `npm audit` (0). |
| **SEC-A6** Docs site | Subagent of `site/src`, `site/dist`, `vercel.json` headers, install-copy JS. |
| **SEC-A7** Input handling | Subagent of SQL, paths, URL parsing, image decode, JSON, subprocess argv. |
| **SEC-A8/A9** Logging & user-trust UX | Reads of `logging.rs`, `bug-report` redact path, `SECURITY.md`. |

## Findings — Medium

### M1 — Third-party GitHub Actions pinned by floating tag, not SHA  (`SEC-A4`)

- **Location**: `.github/workflows/release.yml`
- **Evidence**: every external action is referenced by floating tag — `actions/checkout@v6`, `actions/attest@v4`, `actions/upload-artifact@v7`, `softprops/action-gh-release@v3.0.0`, `Homebrew/actions/setup-homebrew@master`, `taiki-e/install-action@cargo-deny` (alias) in `ci.yml`.
- **Impact**: if any of those upstream repos is compromised (account takeover, malicious maintainer, branch repointing — `@master` is especially risky), the very next release build pulls the compromised step. That step runs with `contents: write` + `id-token: write` + `attestations: write` in the release job, so it could publish a malicious binary with a real SLSA attestation pointing back to this repo.
- **Fix**:
  - Replace every `uses:` with a 40-char commit SHA. Add a comment with the original tag so renovate can update it.
  - In particular, kill `@master` on `Homebrew/actions/setup-homebrew` — that's the worst offender.
- **False-positive notes**: This is industry-default behaviour and most repos do the same. It's still the right thing to fix for a public-distribution binary that ships through Homebrew.

### M2 — `[licenses]` block missing from `deny.toml`  (`SEC-A5`)

- **Location**: `deny.toml` (only contains `[advisories]`).
- **Evidence**: `cargo deny check licenses` rejects ~hundreds of crates because the implicit allowlist is empty. The licenses themselves are fine (MIT / Apache-2.0 / BSD / Unlicense / etc.), but `cargo deny check licenses` is therefore not a useful CI gate.
- **Impact**: a future dep introducing a non-permissive license (GPL/AGPL/SSPL) wouldn't be caught. CI today runs only `check advisories` (`.github/workflows/ci.yml:192`), so this isn't a regression — it's a gap.
- **Fix**: add an `[licenses]` section with an explicit allowlist:
  ```toml
  [licenses]
  allow = ["MIT","Apache-2.0","Apache-2.0 WITH LLVM-exception","0BSD","BSD-2-Clause","BSD-3-Clause","ISC","Zlib","CC0-1.0","Unicode-3.0","Unicode-DFS-2016","MPL-2.0","Unlicense"]
  ```
  Then add `cargo deny check licenses` to `ci.yml`.

### M3 — MCP HTTP bridge: `Origin` header is optional  (`SEC-A3`)

- **Location**: `crates/spotuify-mcp/src/http.rs:114-138`
- **Evidence**: `validate_origin()` returns `Ok(())` when no `Origin` header is present. The `Host` header is not checked at all.
- **Impact**: a non-browser local client (curl, malicious local process) can submit requests without `Origin`. With the bearer token in hand it would already be able to do anything, so the real residual risk is browser-driven DNS-rebinding attacks against a victim who has cached the bearer in a tab — narrow, but a defence-in-depth gap that some auditors will flag.
- **Fix** (one of):
  - Require `Origin` unconditionally for any non-`OPTIONS` request, OR
  - Validate the `Host` header against an allowlist (`127.0.0.1`, `localhost`, `[::1]` with an optional port match).
- **False-positive notes**: with the loopback bind + bearer requirement this is genuinely small. Worth fixing but not blocking.

## Findings — Low

### L1 — macOS binaries not signed or notarized  (`SEC-A4`)

`release.yml` builds and ships unsigned `.tar.gz` for both macOS architectures. README documents the `xattr -d com.apple.quarantine` workaround and points users at the SHA256 checksums + SLSA provenance attestations. This is acceptable for an OSS-distributed tool and is already the chosen posture. Track Apple Developer ID signing as future work.

### L2 — Log directory mode  (`SEC-A8`)

Resolved: `logging.rs` creates the log directory and then sets it to `0700` on Unix. Tracing redacts URIs / search queries (verified in `analytics.rs:572-594`) and the bearer is never logged.

### L3 — Hook command runs through `sh -c` with user-configured string  (`SEC-A7`)

`crates/spotuify-daemon/src/hook_executor.rs:69` runs `Command::new("sh").arg("-c").arg(&cmd)` where `cmd` is `analytics.hook_command` from the config. The Spotify track data flows in as **environment variables** (`SPOTUIFY_TRACK_URI`, etc.) — not interpolated into the shell command — so this is not an injection vector from Spotify data. Risk only materialises if an attacker can already write to the user's config (`spotuify.toml` is enforced `0600` per `config.rs:1016`). Install docs now state that hook commands execute as shell commands exactly as configured.

### L4 — One accepted RustSec advisory  (`SEC-A5`)

- `RUSTSEC-2023-0071` — RSA Marvin Attack via `librespot-core` → `rsa`. Spotuify never exposes attacker-timed RSA verification; ignore is justified in `deny.toml`. Track librespot upstream for a fix.

`RUSTSEC-2024-0384` is resolved by upgrading Tantivy to `0.26.x`, which removes the transitive `instant` dependency. The remaining advisory is explicitly documented in `deny.toml` with a revisit trigger.

### L5 — Three minor `site/` dep lags  (`SEC-A5`)

`@astrojs/starlight` 0.39.0 → 0.39.2, `astro` 6.3.0 → 6.3.6, `sharp` 0.33.5 → 0.34.5. `npm audit` is clean today; just keep Dependabot bumping them.

## Info — Good controls worth flagging

The following are *not* findings — they are working controls the auditor will want to see called out:

- **OAuth PKCE (S256) + state validation** — `crates/spotuify-spotify/src/auth.rs:129-130, 1112-1121`. 32-byte state, 96-byte verifier, SHA-256 challenge.
- **Redirect URI is loopback-only** — `auth.rs:1047-1049` rejects any non-loopback host before binding.
- **Credential storage is explicit and mode-restricted** — default dev-app OAuth credentials are stored under `<config_dir>/auth/token.json`; first-party/keymaster opt-in stores refresh token + scopes under `<config_dir>/auth/first-party.json`. On Unix the auth directory is `0700`, auth files are written atomically with mode `0600`, and cross-process refresh is guarded by `<config_dir>/auth/token.lock`.
- **`spotuify auth bearer` requires `--reveal-secret`** — fail-safe default; verified at `src/main.rs:2508-2509`.
- **Config + token files mode `0600` on Unix** — `crates/spotuify-spotify/src/config.rs:1011-1025` (also `set_permissions` after write).
- **Auto-generated `.gitignore` in config dir** — `config.rs:1030-1043` guards against dotfile-sync uploads.
- **Bug-report redaction** — `src/main.rs:2320-2360` strips `client_secret/token/refresh_token/password/api_key` and email addresses before bundling.
- **Daily log rotation, 7-file retention** — `logging.rs:45-50`.
- **Daemon socket path** — `crates/spotuify-protocol/src/paths.rs:117-160`. Uses `$XDG_RUNTIME_DIR` / `~/Library/Application Support`, falls back to `/tmp/spotuify-<uid>/`. Not a world-writable predictable path.
- **IPC frame cap** — `LengthDelimitedCodec::max_frame_length(16 MB)` in `spotuify-protocol/src/lib.rs`.
- **MCP loopback bind + bearer token** — `crates/spotuify-mcp/src/http.rs:43-51, 99-112`.
- **MCP destructive tools require `confirm: true`** — `crates/spotuify-mcp/src/confirm.rs:38-54`. Unconfirmed calls return preview-only.
- **All HTTP timeouts bounded** — `connect_timeout(4s)`, `read_timeout(8s)`, `timeout(8s)` in `spotuify-spotify/src/client.rs:119-126`; cover-art `timeout(15s)`; diagnostics `10s/20s`; hook executor enforces `100ms` floor.
- **Cover-art decoder hardened** — `crates/spotuify-system/src/cover_cache.rs:226-242` enforces 10 MB byte cap, content-type allowlist (`image/jpeg|png|webp`), 32×32 min dimensions, atomic `.tmp → rename`.
- **All SQL parameterised** — `sqlx::query!` / `?` placeholders across `spotuify-store`. No `format!`-built SQL.
- **Workspace lints** — `unsafe_code = "deny"`, `unused_must_use = "deny"`, `unwrap_used = "warn"`. No `#[allow(unsafe_code)]` overrides found.
- **No git deps, no external path deps** — all crates from crates.io.
- **No workspace `build.rs`** — only mainstream third-party crates have build scripts.
- **CI gates** — `.github/workflows/ci.yml` runs `cargo deny check advisories` (line 192) and `npm audit --omit=dev --audit-level=moderate` (line 210).
- **Release workflow scoping** — tag-triggered only, regex-validated, minimal `permissions:` per job, no `pull_request_target` paths.
- **Build provenance** — `actions/attest@v4` emits SLSA attestations alongside `.sha256` files for every release archive.
- **Homebrew formula SHA256s computed from real artifacts** — `scripts/render_homebrew_formula.sh:15-23` reads each `.sha256` file and substitutes into the templated formula.
- **No autostart by default** — `launchd`/`systemd`/`schtasks` units are only registered when the user explicitly runs `spotuify daemon install-service`.
- **No binary self-updater** — current update-awareness only checks the public GitHub releases API and reports how to upgrade. It does not download or replace the running binary.
- **No telemetry** — no Sentry/Mixpanel/Amplitude/PostHog/Segment/GA SDKs in `Cargo.toml`. Analytics layer is local-SQLite-only.
- **Site security headers** — `site/vercel.json:7-36` ships CSP, HSTS, Referrer-Policy, Permissions-Policy, X-Content-Type-Options, X-Frame-Options, and `upgrade-insecure-requests`.
- **No site DOM-sink hazards** — no `eval`, `innerHTML`, `dangerouslySetInnerHTML`, `Function(`, `document.write`. `install-copy.js` uses `textContent` + `navigator.clipboard.writeText`.
- **No secrets in built site bundle** — grep on `site/dist/_astro/*.js` clean for `TOKEN/SECRET/API_KEY/process.env`.
- **`SECURITY.md`** present with private reporting guidance and a "no secrets in public issues" rule.

## What an external reviewer will probably ask, and the short answer

| Question | Answer |
|---|---|
| Why is the macOS binary unsigned? | Tracked as L1. README documents `xattr` workaround and points at SHA256+SLSA provenance. Apple Developer ID signing is future work; this is normal for OSS Rust CLIs. |
| Does it phone home? | No telemetry, analytics SDK, or crash reporter. Current builds do make a bounded public GitHub releases API check for update-awareness; disable it with `SPOTUIFY_NO_UPDATE_CHECK=1`. Music network egress remains `*.spotify.com`, `*.scdn.co`, librespot AP, and optional `lrclib.net`. |
| Does it autostart? | Only if the user runs `spotuify daemon install-service`. The installer scripts under `install/launchd/`, `install/systemd/`, `install/windows/` are vetted; they create *user-level* units, not system units, and are removable with one command. |
| Does it self-update? | No. It can report that a newer release exists, but updates are still via Homebrew (`brew upgrade`), Cargo, DMG download, or manual GitHub Releases install. |
| Where are my Spotify tokens? | Default dev-app auth stores the OAuth token under `<config_dir>/auth/token.json`, guarded by `<config_dir>/auth/token.lock`. First-party/keymaster opt-in stores refresh token + scopes under `<config_dir>/auth/first-party.json`. On Unix the auth directory is `0700` and files are `0600`. |
| Why is RSA RUSTSEC-2023-0071 not fixed? | Transitive via librespot. Not exploitable in our usage pattern. Documented in `deny.toml` with revisit trigger. |
| Can a website on `http://127.0.0.1:NNN` attack the daemon? | The IPC daemon is local-only: Unix socket on Unix, named pipe on Windows, no network bind. The MCP HTTP bridge requires a Bearer token from `SPOTUIFY_MCP_TOKEN`. A defence-in-depth gap (M3 — optional `Origin`) is being tracked. |

## Recommended fix order

1. **M1 — pin Actions to SHAs.** One PR. Touch every `uses:` in `.github/workflows/*.yml`. Drop `@master` on `Homebrew/actions/setup-homebrew` first.
2. **M2 — add `[licenses]` to `deny.toml` and run `cargo deny check licenses` in CI.** Small.
3. **M3 — require `Origin` (or validate `Host`) on the MCP HTTP bridge.** Adjust the one function in `crates/spotuify-mcp/src/http.rs`.
4. **L2 — `chmod 0700` log dir on Unix.** Two lines in `logging.rs`.
5. **L1, L4, L5** — track, do not block.

## Coverage gaps in this audit

The following were *not* exercised and are good candidates for a future pass:

- **Dynamic / runtime testing**: no live daemon was driven; no socket-permission spot-check on a real install; no traffic capture against `spotuify search/play`.
- **Fuzzing**: no fuzz harness was run against the IPC codec or cover-art decoder. Both have reasonable static bounds (`max_frame_length`, `MAX_COVER_ART_BYTES`) but a one-shot fuzzing campaign would be cheap insurance.
- **librespot upstream**: treated as a trust boundary, not re-audited. RSA + RUSTSEC-2023-0071 lives there.
- **`agent-browser` / `playwright` skills, MCP clients**: out of scope for the binary audit.

## Closing

You can publish `spotuify 0.1.24` to Homebrew, GitHub Releases, and the docs site without expecting AV / Gatekeeper / registry-reviewer pushback beyond the standard "unsigned macOS binary" prompt — which the README already addresses. The three Medium items are worth landing in `0.1.25` because they make the project look more careful to anyone glancing at the repo, but none of them blocks the current release.
