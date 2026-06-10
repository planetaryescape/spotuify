# Spotuify Public-Release Security Audit Rubric (v2)

> Independent rubric for the second-pass security audit prior to public distribution. Companion to the audit report in `security-audit-v2-report.md`. Standalone from `security-audit-rubric.md` (v1) so the two passes don't contaminate each other.

## Purpose

`spotuify` ships as:

- A native binary distributed via Homebrew, `cargo install`, and GitHub Releases (macOS, Linux, Windows x64).
- A static documentation site at `spotuify.vercel.app`.
- A background daemon with local IPC, a loopback MCP HTTP bridge, and an embedded librespot.

A reviewer at a distribution registry (Homebrew core, App Store, antivirus engine, OS Gatekeeper) will ask three questions. This rubric is organised around them:

1. **Is the binary doing only what a Spotify controller should do?** (no telemetry, no persistence, no surprise network calls)
2. **Are user secrets safe?** (OAuth tokens, refresh tokens, live bearers, listening history)
3. **Is the distribution path trustworthy?** (release pipeline, install scripts, supply chain)

## Severity

| Level | Meaning | Examples |
|-------|---------|----------|
| **Critical** | Likely credential theft, arbitrary code execution, or active malicious behaviour. Blocks public release. | Hardcoded credential in release artifact. Curl-pipe-shell installer with no verification. Network-exposed IPC with no auth. |
| **High** | Realistic exploitation path, plausible AV/store flag, or trust-bypass. Should fix before publishing. | Floating action tag in release workflow. Daemon socket world-readable. Bearer logged to disk. |
| **Medium** | Defence-in-depth gap that an auditor will note but isn't directly exploitable. Fix in next release. | Missing CSP on docs site. Image decoder without size cap. Action pinned by tag not SHA. |
| **Low** | Hygiene / documentation / provenance gap. Track and improve. | No SECURITY.md private channel. README doesn't mention checksums. Missing `Permissions-Policy` header. |
| **Info** | Observation worth recording, not a finding. | "OAuth flow uses PKCE — good." |

## Evidence Standard

Every finding includes:

- **Rule ID** (e.g. `SEC-02 / Release pipeline`)
- **Severity**
- **Location**: `path/to/file.rs:LINE` or `.github/workflows/release.yml:LINE`
- **Evidence**: code excerpt, command output, screenshot
- **Impact**: what an attacker / reviewer / user actually loses
- **Recommended fix**: concrete, minimal
- **False-positive notes**: context that could downgrade the finding

`Info` items skip impact/fix.

## Rubric Categories

### SEC-A1 — Credentials & Secrets

**Goal**: no secrets in the source tree, no secrets in release artifacts, runtime secrets minimised and protected.

Pass criteria:

- No OAuth tokens, refresh tokens, bearers, passwords, private keys, signing keys, or API client secrets committed anywhere in the repo, docs, fixtures, tests, packaging, GitHub workflows, or the site bundle.
- Spotify OAuth uses PKCE; `state` is validated; redirect URI is restricted to loopback.
- Long-lived credentials stored in private auth files with `0600` perms on Unix.
- Live Web API bearer is **not** persisted to disk. CLI surfaces that print secrets require an explicit `--reveal-secret`-style flag.
- Log files, `bug-report` bundles, `doctor` output, and CLI human output redact `Authorization`, `refresh_token`, `client_secret`, and similar fields by default.
- Config files containing user-tagged data are written `0600` on Unix.

Tooling:

- `rg -i 'secret|token|bearer|password|api[_-]?key|client[_-]?secret' --hidden -g'!target' -g'!node_modules' -g'!.git'`
- `rg -aE 'eyJ[A-Za-z0-9_-]{20,}|gho_|ghp_|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16}|BEGIN (RSA|EC|OPENSSH|PRIVATE)'`
- inspect built site bundle: `rg -F 'client_id' site/dist/`

### SEC-A2 — Network & Process Behaviour

**Goal**: the binary should only contact Spotify endpoints and the optional lyrics provider; should not silently exfiltrate, install autostart, or spawn unexpected processes.

Pass criteria:

- All outbound network destinations are documented in README/AGENTS and limited to: `*.spotify.com`, `*.scdn.co`, librespot AP/dealer servers, the configured lyrics provider, and (optionally) `api.github.com` for explicit `spotuify update`. No analytics / telemetry / Sentry / crash reporting wired in.
- All `Command::new(...)` shell-outs are listed and bounded. No `sh -c "...{user_input}"` patterns.
- Files are only written under XDG/`~/Library/Application Support`/`~/Library/Caches` (or equivalent) — no writes to `/usr/local`, `/etc`, `~/Library/LaunchAgents`, etc., unless the user explicitly opts in via `spotuify daemon install`.
- Autostart units (`launchd`, `systemd`) require explicit user action; never installed by default install.
- No self-updater that downloads and executes a new binary without checksum/signature verification.
- Embedded librespot is constrained to Spotify Connect endpoints (verified upstream).

Tooling:

- `rg -oE 'https?://[a-zA-Z0-9._/-]+' src crates | sort -u`
- `rg -nE 'Command::new|process::Command|tokio::process'`
- `rg -nE 'LaunchAgents|systemd|StartupItems'`

### SEC-A3 — Local Trust Boundaries (Daemon IPC & MCP)

**Goal**: the daemon is local-only; nothing on the network can talk to it; another local user can't impersonate the owner.

Pass criteria:

- Unix socket lives in `$XDG_RUNTIME_DIR` (Linux) or `~/Library/Application Support/spotuify` (macOS), socket mode `0600`, parent dir `0700`.
- Windows IPC uses a local named pipe path and does not expose the daemon on TCP.
- IPC frames are length-prefixed with a bounded `max_frame_length`.
- MCP HTTP bridge binds to `127.0.0.1` by default. Bearer / token required on every request. Token compared in constant time.
- MCP bridge validates the `Host` header against an allowlist (defeats DNS-rebinding). CORS is not `*`.
- SQLite DB, search index, and log files are mode `0600` or `0700`.
- Destructive MCP/IPC tools (playlist delete, library wipe, bulk follow) require an explicit confirmation/dry-run flag.

Tooling:

- `rg -n 'UnixListener|named_pipe|bind|set_permissions|max_frame_length|TcpListener::bind' crates`
- `rg -n '127\.0\.0\.1|0\.0\.0\.0|::' crates`

### SEC-A4 — Public Distribution Trust

**Goal**: a user installing via `brew install`, `cargo install`, or downloading a release archive can trust they got the bits the maintainer published.

Pass criteria:

- Release workflow runs only on tag pushes from the canonical repo. Jobs declare minimal `permissions:` (default-deny; `contents: write` only where needed; `id-token: write` only for attestation).
- No `pull_request_target` jobs run untrusted PR code with release secrets. Workflow body has no `${{ github.event.* }}` shell interpolation.
- All third-party actions pinned by **commit SHA**, not by floating tag (`@master`/`@v4`).
- Each release artifact has a published `.sha256` file and ideally a GitHub artifact-provenance attestation (SLSA L2+).
- Homebrew formula contains the SHA256 of each artifact and is generated from the actual published archive (not pre-computed).
- Install instructions are explicit: no `curl … | sh` without a checksum-verification step, or the README explicitly recommends downloading and inspecting the script first.
- The release workflow refuses to re-tag or overwrite an existing release.
- Source archives are reproducible enough that a third party can rebuild from the tag and get the same artifact (within Rust's determinism limits).
- macOS: either signed + notarized, or the README explicitly explains the quarantine prompt and how to verify checksums.

### SEC-A5 — Supply Chain & Dependencies

**Goal**: known-bad upstream code does not ship in the binary; lockfiles are honoured; CI gates dependency drift.

Pass criteria:

- `cargo deny check advisories` runs in CI and gates merges. Any ignored advisory has a written reason in `deny.toml` and a revisit trigger (next dep upgrade).
- `npm audit --omit=dev --audit-level=moderate` runs in CI for `site/`.
- Lockfiles checked in (`Cargo.lock`, `site/package-lock.json`).
- Dependabot or Renovate configured for both ecosystems.
- No floating git deps (no `git=` lines in Cargo.toml outside vendored upstreams with pinned rev). Path deps only for in-repo workspace members.
- `build.rs` scripts from non-mainstream crates are listed and reviewed.
- No `unmaintained` crates ignored without a tracked upgrade plan.

### SEC-A6 — Static Docs Site

**Goal**: `spotuify.vercel.app` doesn't get the project flagged by browser security scanners.

Pass criteria:

- `Content-Security-Policy` header set; `default-src 'self'` + minimal whitelists. No `unsafe-inline` script src.
- `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy: ()`, `Strict-Transport-Security: max-age=63072000; includeSubDomains; preload` all set.
- No `eval`, `Function(`, `innerHTML` + user input, `document.write` in site source or built JS.
- External scripts loaded with `integrity=` (SRI) when feasible, or self-hosted.
- No mixed content (no `http://` resources).
- Install copy / quickstart commands do not pipe unverified scripts into shells.
- No development secrets or env vars baked into the built bundle.

### SEC-A7 — Input Handling & Robustness

**Goal**: malformed local-IPC requests, malicious Spotify API responses, or huge cover-art images can't crash the daemon or escalate to RCE.

Pass criteria:

- SQL queries parameterised (`sqlx::query!` / `?` placeholders). No `format!` building SQL with user input.
- Paths from user input (config, IPC, CLI) canonicalised and validated against traversal.
- Cover-art / image fetches are size-limited *before* decode, content-type checked, and decoded with a bounded crate (`image` with explicit codec features).
- `serde_json::from_slice` of network input has size limits at the reader layer.
- No `sh -c "{user_input}"` patterns in subprocess construction.
- `unsafe_code = "deny"` at workspace level; no per-crate overrides without justification.
- External calls (Spotify Web API, librespot, lyrics) have explicit timeouts. Default `reqwest::Client::new()` (no timeout) is **not** used for outbound calls.

### SEC-A8 — Logging, Diagnostics & User-Trust UX

**Goal**: when a user shares a `bug-report` bundle, daemon log, or screenshot, they don't accidentally leak credentials. Error messages don't reveal exploitable internals.

Pass criteria:

- Tracing layer redacts `Authorization`, `Cookie`, `refresh_token`, query-string tokens, and Spotify URIs that include user IDs (low priority).
- `spotuify bug-report` collects relevant state but strips secrets and asks for review before upload/share.
- `spotuify doctor` doesn't print full tokens or full URLs with embedded auth.
- Crash reports (panic hooks, if any) don't include token-bearing variables.
- `--verbose` levels are documented; `--debug` doesn't accidentally unlock secret printing.

### SEC-A9 — Documentation & User-Facing Signals

**Goal**: a security-conscious user can verify integrity, understand the trust model, and report issues privately.

Pass criteria:

- `SECURITY.md` exists with a private reporting channel (GitHub private advisories or email).
- README documents: where secrets are stored, what network endpoints are contacted, how to verify checksums, and any unsigned-binary caveats.
- LICENSE present and consistent.
- Release notes mention security-relevant changes.
- Privacy posture documented (no telemetry, what is stored locally and where).

## Audit Method

1. **Scope sweep** — enumerate every binary, daemon surface, network egress, install path.
2. **Static checks** — run scanners (`cargo deny`, `npm audit`, ripgrep secret patterns, ripgrep dangerous sinks).
3. **Targeted code review** — credentials, OAuth, IPC, MCP, release workflow, install scripts, image decode, SQL.
4. **Behavioural verification** — where practical, drive `spotuify` (CLI binary), inspect socket permissions, capture network traffic, read log redaction.
5. **Report** — findings keyed to rule IDs, severity-ordered, with concrete fixes and a final ship/hold verdict.

## Out of Scope

- Spotify upstream API security (not under maintainer's control).
- librespot internals beyond confirming its network shape (audit upstream separately).
- General Rust safety (`unsafe` already lint-denied workspace-wide; no fuzz harness expected).
- Hardware attacks, side-channels, physical access scenarios.

## Verdict Bands

- **Ship now** — no Critical or High findings.
- **Ship with caveats** — no Critical, ≤2 High with disclosed mitigation in README.
- **Hold** — any Critical, or ≥3 High, or a single Medium that affects the install path.
