# Bugs And Security Audit

Worktree: `/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security`
Branch: `codex/audit-bugs-security-20260619`
Date: 2026-06-19
Scope: bugs, security, validation, auth/token handling, IPC/MCP safety, file/permission handling, unsafe assumptions, and user-facing panic/error paths. No fixes implemented.

## Summary

Findings: 6 total: 1 High, 3 Medium, 2 Low.

No repository secrets were reproduced. Evidence below is limited to source locations and behavior.

## Findings

### BS-01 - High - `radio_start` can mutate playback queue over MCP without destructive confirmation

- Priority: High
- Issue: MCP exposes `radio_start` as a non-destructive transport tool, defaults `dry_run` to `false`, and the daemon queues returned station tracks directly.
- Evidence:
  - [tools.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/tools.rs:233) declares `radio_start` with `kind: ToolKind::Transport` and `destructive: false`.
  - [confirm.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/confirm.rs:29) only requires confirmation when the tool is marked destructive.
  - [bridge.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/bridge.rs:291) maps missing `dry_run` to `false`.
  - [library.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/handlers/library.rs:171) queues every returned `track_uri` when `dry_run` is false.
  - [library.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/handlers/library.rs:183) shows nearby library mutations use optimistic operation tracking, but `RadioStart` does not.
- Impact: A mistaken or prompt-injected MCP call can enqueue many tracks against the active Spotify session without an explicit mutation confirmation, receipt, or undo trail.
- Recommended action: Treat `radio_start` as destructive when `dry_run` is false, require MCP confirmation, and default MCP calls to preview mode. Route queueing through the same operation/receipt machinery used by other mutations, or document why queue mutations are intentionally excluded.
- Confidence: High.
- Validation idea: Add an MCP tool-call test where `radio_start` without `confirm: true` returns a confirmation error, and a daemon test proving non-dry-run queue additions create an operation record or receipt.

### BS-02 - Medium - Cover-art IPC fetches arbitrary URLs from the daemon

- Priority: Medium
- Issue: `Request::Image` and `Request::CoverArt` accept a raw URL string and the daemon fetches it with `reqwest` without scheme, host, redirect, or private-network allowlisting.
- Evidence:
  - [lib.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-protocol/src/lib.rs:188) defines `Image { url: String }` and `CoverArt { url: String }`.
  - [media.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/handlers/media.rs:16) passes the client URL directly to `cover_cache.get_or_fetch_entry`.
  - [cover_cache.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-system/src/cover_cache.rs:126) validates only empty input before fetching.
  - [cover_cache.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-system/src/cover_cache.rs:203) performs `http.get(url).send()`.
  - [app.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-tui/src/app.rs:2873) shows normal TUI callers use album image URLs, but the daemon IPC accepts any local client value.
- Impact: Any local IPC client can make the daemon issue HTTP requests to loopback, LAN, link-local, or other unintended targets. That is a local-user SSRF primitive and can also cache attacker-controlled responses.
- Recommended action: Restrict cover fetches to `https` and known Spotify image hosts, reject loopback/private/link-local IPs, and ensure redirects cannot leave the allowlist. Keep content-type and size checks, but perform origin validation before the request.
- Confidence: High.
- Validation idea: Start a local HTTP server and send `CoverArt { url: "http://127.0.0.1:PORT/x.jpg" }`; the daemon should reject before any request reaches the server. Add an allowed-host regression test with a Spotify CDN URL.

### BS-03 - Medium - Public limits are unbounded at the daemon boundary

- Priority: Medium
- Issue: Several CLI/protocol paths accept large `u32`/`usize` limits and the daemon/store use them directly for SQLite reads, memory accumulation, and Spotify pagination.
- Evidence:
  - [cli_args.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-cli/src/cli_args.rs:399) exposes `library saved-tracks --limit u32` without a range.
  - [main.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/src/main.rs:195) exposes history `limit: u32` without a range.
  - [library.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/handlers/library.rs:24) converts `limit` to `usize` and loops Spotify requests until enough saved tracks are collected.
  - [handler.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/handler.rs:574) passes search `limit` through to local/Spotify search after only capping query length.
  - [lib.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-store/src/lib.rs:302) binds the search `LIMIT` directly.
  - [operations.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-store/src/operations.rs:201) binds operations-log `LIMIT` directly.
- Impact: A local client can request very large result sets, driving long SQLite scans, high memory usage, repeated Spotify API calls, and rate-limit pressure. MCP caps some bridge-level limits, but the daemon is the real trust boundary.
- Recommended action: Add central daemon/protocol caps per request class, mirror them in clap value parsers, and return explicit validation errors rather than silently allowing huge limits.
- Confidence: High.
- Validation idea: Add request-level tests for `u32::MAX`/`usize::MAX` limits on search, saved tracks, history, and ops log. Verify they fail fast or clamp to documented maxima before store/API calls.

### BS-04 - Medium - macOS updater verifies release hashes but not app signing identity

- Priority: Medium.
- Issue: The macOS updater downloads a DMG and `.sha256`, checks the hash, mounts it, and swaps in `Spotuify.app`; it does not verify Developer ID signature, team identifier, or notarization state before install.
- Evidence:
  - [AppUpdater.swift](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:70) constructs release asset URLs and verifies only the downloaded SHA-256.
  - [AppUpdater.swift](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:87) mounts the DMG and stages the contained app after the hash check.
  - [AppUpdater.swift](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:193) swaps the staged app into place.
  - [build-dmg.sh](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/clients/macos/scripts/build-dmg.sh:6) documents that signing/notarization is conditional.
  - [build-dmg.sh](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/clients/macos/scripts/build-dmg.sh:237) signs/notarizes only when signing identity/profile configuration is present.
- Impact: If the GitHub release asset channel is compromised, an attacker can publish a malicious DMG plus matching checksum and the updater will accept it. Hash checking protects transit integrity, not publisher identity.
- Recommended action: Before staging or swapping, require `codesign --verify --deep --strict`, verify the expected Team ID/designated requirement, and preferably require successful Gatekeeper/notarization assessment. Reject unsigned/ad-hoc builds in the updater path.
- Confidence: Medium.
- Validation idea: Build or craft an unsigned/ad-hoc `Spotuify.app` DMG with a correct `.sha256`; updater should reject it before staging.

### BS-05 - Low - MCP bearer token comparison is not constant time

- Priority: Low.
- Issue: MCP HTTP auth compares the full `Authorization` header to `Bearer {token}` with normal string equality, despite the security rubric calling for constant-time token comparison.
- Evidence:
  - [http.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/http.rs:24) requires `SPOTUIFY_MCP_TOKEN` for HTTP transport.
  - [http.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/http.rs:100) builds `expected = format!("Bearer {}", state.token)`.
  - [http.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-mcp/src/http.rs:102) uses `actual == Some(expected.as_str())`.
  - [security-audit-rubric-v2.md](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/docs/security-audit-rubric-v2.md:92) explicitly requires constant-time bearer/token comparison.
- Impact: Practical exploitability over loopback is low, but this violates the stated security contract and is cheap to fix.
- Recommended action: Parse the `Bearer ` prefix, compare only the presented token bytes with a constant-time equality implementation such as `subtle` or `constant_time_eq`, and keep the existing Host/Origin checks.
- Confidence: High.
- Validation idea: Unit test accepted/rejected bearer values through the auth middleware, then inspect implementation to ensure constant-time comparison is used for equal-length candidate tokens.

### BS-06 - Low - Hook commands can leak embedded secrets through logs and bug reports

- Priority: Low.
- Issue: User hook commands are logged verbatim on timeout/failure, and `bug-report` includes raw log tails without log redaction. Hook commands often contain webhook URLs or bearer-like query parameters.
- Evidence:
  - [hook_executor.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-daemon/src/hook_executor.rs:80) logs `command = %cmd` on timeout/failure/non-zero exit.
  - [hooks.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/crates/spotuify-system/src/hooks.rs:224) includes the full configured command in spawn errors.
  - [main.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/src/main.rs:2516) adds raw `spotuify.log` tail content to the diagnostic tarball.
  - [main.rs](/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/bugs-security/src/main.rs:2549) redacts config values, but that redaction is not applied to logs.
- Impact: A user who embeds a webhook token, API key, or signed URL in a hook command can leak it into daemon logs and then into a bug-report tarball they may share for support.
- Recommended action: Avoid logging full hook commands. Log only the executable name, a stable command hash, or a redacted command. Apply the same secret-redaction pass, plus URL credential/query-token redaction, to log lines included in bug reports.
- Confidence: Medium.
- Validation idea: Configure a failing hook command containing `token=secret-value`, run the hook test or daemon path, generate a bug report, and assert the token string does not appear in logs or the tarball.
