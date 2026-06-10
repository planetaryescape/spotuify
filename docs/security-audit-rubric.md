# Security Audit Rubric

Scope: `spotuify` public distribution review for source, release automation, installer surfaces, the static documentation site, and local runtime security.

This rubric prioritizes issues that could make the project look malicious, expose user credentials, let untrusted local or remote input execute code, or weaken distribution trust.

## Severity

- Critical: likely credential compromise, arbitrary code execution without clear user intent, malicious release path, or remotely exploitable vulnerability.
- High: plausible credential leak, local privilege or trust-boundary bypass, unsafe updater/installer behavior, or dangerous public artifact.
- Medium: defense-in-depth gap likely to be flagged by audit tooling or manual review.
- Low: hardening, documentation, provenance, or hygiene gap with limited direct exploitability.

## Evidence Standard

Every finding must include:

- Rule ID
- Severity
- Location with line numbers
- Evidence from code, config, docs, command output, or dependency scanner
- Impact
- Recommended fix
- False-positive notes where context may change the result

## Rubric

### SEC-01: Secrets And Credential Handling

Pass criteria:

- No committed OAuth tokens, refresh tokens, bearer tokens, passwords, private keys, signing keys, or API secrets.
- Client-side/site code does not embed secrets in public bundles or static assets.
- Runtime credentials are stored in private auth files with restrictive permissions.
- Logs, diagnostics, bug reports, and CLI output redact secrets by default.

Audit steps:

- Search for secret-like literals and known credential field names.
- Inspect auth/credential storage code, logging, diagnostics, and bug-report output.
- Inspect docs and generated site content for accidental token examples.

### SEC-02: Public Distribution Trust

Pass criteria:

- Release workflows do not run untrusted code with release secrets.
- Release artifacts are built from intended refs/tags and are not overwritten.
- Install scripts/formulae do not pipe remote code into shells without verification.
- Checksums, provenance, or signatures exist where practical.
- Package metadata does not misrepresent scope, install behavior, or privileges.

Audit steps:

- Inspect GitHub Actions triggers, permissions, token use, artifact upload, Homebrew generation, and release notes.
- Inspect scripts and docs for curl-pipe-shell, broad filesystem writes, unexpected daemon/autostart behavior, or misleading install instructions.

### SEC-03: Dependency And Supply-Chain Health

Pass criteria:

- Rust and JavaScript dependencies have no known critical/high advisories.
- Lockfiles are present for shipped builds.
- Dependency automation is enabled.
- Native/build-script heavy dependencies are justified.

Audit steps:

- Run available audit tooling for Cargo and npm.
- Inspect lockfiles and dependency update config.
- Identify high-risk dependencies and required follow-up if scanners are unavailable.

### SEC-04: Local Trust Boundaries And IPC

Pass criteria:

- Daemon IPC is local-only and not exposed to the network unless explicitly configured.
- Local sockets/files are created under user-owned runtime directories with restrictive permissions.
- Request handlers validate input and avoid hidden privileged operations.
- MCP or HTTP bridges bind to loopback by default and document any wider exposure.

Audit steps:

- Inspect protocol paths, server binds, socket cleanup, MCP HTTP server, and daemon command handlers.
- Check for unauthenticated network listeners.

### SEC-05: Command Execution And Hooks

Pass criteria:

- No shell execution of untrusted strings.
- User-configured hooks are explicit, documented, and disabled or scoped by default.
- Commands use argv-style execution instead of shell interpolation.
- Environment and working directory handling do not leak secrets unnecessarily.

Audit steps:

- Search for `Command`, shell invocations, hook execution, scripts, and docs.
- Trace all command arguments back to their source.

### SEC-06: Filesystem, Paths, And Permissions

Pass criteria:

- Config, cache, logs, socket, database, and index paths stay within intended user directories by default.
- User-provided paths are canonicalized or constrained where they cross trust boundaries.
- Temporary files/directories use safe APIs.
- Destructive operations require explicit intent or dry-run where feasible.

Audit steps:

- Inspect path construction, cache reset/repair, export/import, log tailing, and temp file use.
- Check for broad delete/write behavior.

### SEC-07: Network And External API Safety

Pass criteria:

- HTTP clients use TLS verification and bounded timeouts.
- OAuth and Spotify API calls do not log credentials.
- Redirect/callback handling validates expected shape.
- Rate limiting and retry behavior avoid abusive traffic.

Audit steps:

- Inspect reqwest clients, OAuth flow, callback listener, Spotify endpoints, retries, and bearer-token surfaces.

### SEC-08: Static Site And Browser Security

Pass criteria:

- No dangerous DOM sinks with untrusted input (`innerHTML`, `document.write`, `eval`, string timers, unsafe `postMessage`).
- No public secrets in frontend env/config/assets.
- External scripts/styles are minimized and protected through SRI or hosting controls where possible.
- Production deployment defines expected security headers: CSP, frame protections, `nosniff`, referrer policy, and permissions policy.

Audit steps:

- Inspect Astro config, site source, scripts, generated HTML assumptions, dependencies, and deployment config.
- Treat headers not visible in repo as a runtime verification item.

### SEC-09: Data Import, Export, And Diagnostics

Pass criteria:

- Imports validate format and size before processing.
- Exports and bug reports do not include secrets by default.
- Logs and diagnostics disclose only necessary local state.
- JSON/CSV output remains parseable without injection-prone formatting.

Audit steps:

- Inspect analytics import/export, cache repair/reset, bug report, logs, and doctor commands.

### SEC-10: Abuse Resistance And User Consent

Pass criteria:

- Playback, queue, playlist, follow/like, hook, notification, Discord, and system-integration actions require explicit user command/config.
- Broad mutations support dry-run or confirmation where feasible.
- Background daemon behavior is transparent and stoppable.
- No hidden telemetry or unexpected network destinations.

Audit steps:

- Inspect commands, default config, system integration defaults, docs, and daemon startup behavior.

### SEC-11: Rust Memory-Safety Posture

Pass criteria:

- Workspace denies unsafe code unless a crate-level exception is explicitly justified.
- Panics/unwraps do not cross untrusted input paths in ways that cause exploitable denial-of-service.
- Parsing and IPC decode paths fail closed with bounded payload sizes.

Audit steps:

- Search for `unsafe`, `unwrap`, `expect`, unchecked indexing, unbounded reads, and JSON frame limits.

### SEC-12: Auditability And Release Readiness

Pass criteria:

- Security policy/contact is present or planned before broad distribution.
- Security-sensitive behavior is documented clearly.
- CI runs tests and dependency checks appropriate for release.
- Known gaps have owner, severity, and remediation path.

Audit steps:

- Inspect docs, README, workflows, and generated reference docs.
