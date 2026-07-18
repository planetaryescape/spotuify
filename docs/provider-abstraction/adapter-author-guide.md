# Provider Adapter Author Guide

This guide is the implementation contract for adding a provider adapter to
`spotuify`. The reference implementation is
`crates/spotuify-provider-fake`; the executable contract is its conformance
suite. Provider-native API shapes, authentication, and quirks stay inside the
adapter and daemon factory.

Adding an adapter is a product decision. Do not register a real service until
its user value, legal/API constraints, and supported capabilities are recorded
in `docs/blueprint/13-decision-log.md`.

## Contract map

| Facet | Implement when | Capability source |
|---|---|---|
| `MusicProvider` | Always | `ProviderCaps::{search,catalog,library,playlists}` |
| `RemoteTransport` | The provider exposes remote playback control | `ProviderCaps::transport = Some(...)` |
| `ProviderExtras` | The provider has semantic workflows such as native lyrics, related artists, or radio | `ProviderExtrasCaps` supplied by the installed extras facet |
| `PlayerBackend` | The provider supplies a local playback device/backend | Backend methods plus provider/URI identity |

Absence is meaningful. A metadata-only adapter implements `MusicProvider`,
declares `transport: None`, and installs no transport, extras, or player
object. Do not install null implementations that fail every call.

## 1. Identity and URIs

Choose two identities deliberately:

- `ProviderId` identifies one configured adapter instance. It must contain
  lowercase ASCII letters, digits, or hyphens and start with a letter.
- `UriScheme` owns a canonical resource namespace. Every resource crossing
  the adapter boundary uses `ResourceUri` in the form
  `<scheme>:<kind>:<provider-item-id>`.

A registry rejects duplicate provider IDs, duplicate URI schemes, and facet
identity mismatches. Every returned `MediaItem`, `Playlist`, mutation receipt,
transport result, and extras result must stay in the adapter's URI namespace.
Use `ResourceUri::new`/`ResourceUri::parse`; do not assemble or split URIs with
string operations.

`MusicProvider::claim_target` owns provider share URLs and legacy input. Its
tri-state result matters:

- `NotMine`: ordinary search text or another provider's target.
- `Resolved`: a canonical URI owned by this adapter.
- `Invalid`: the adapter recognized its namespace but the input is malformed.

Never claim another adapter's input, and never silently reinterpret malformed
provider URLs as search queries.

## 2. Implement `MusicProvider`

Keep `ProviderCaps` truthful and specific. All fields default to unsupported;
enable only operations the adapter implements. Declare per-kind support and
real page/batch limits. The daemon gates on capabilities, while default trait
methods still return typed `ProviderError::Unsupported` as a defensive layer.

For every read:

1. Validate resource scheme, media kind, page size, and adapter-specific
   limits before remote I/O.
2. Honor `RequestContext::priority` in the adapter's rate/concurrency limiter.
3. Return `ProviderPage::requested_offset` exactly as requested. Preserve a
   provider cursor as `PageContinuation::Cursor`; do not parse opaque cursors.
4. Return canonical provider-owned items. Totals may be `None` when the API
   cannot calculate them.
5. Return `Ok(None)` from optional lookup methods such as `media_item` and
   `playlist` when the resource does not exist. Use
   `AccessOutcome::Unavailable` when a collection exists but is unreadable;
   reserve `ProviderError::NotFound` for non-optional operations whose target
   disappeared.

Freshness probes and playlist version tokens are opaque adapter values. The
store persists them, but only the adapter interprets them through
`library_freshness_changed` and `playlist_version_changed`.

All writes enter through `apply_mutation(mutation_id, mutation)`. The daemon's
durable operation claim is the idempotency authority; the adapter may retain
receipts for best-effort suppression but must not claim remote exactly-once
semantics it cannot guarantee. A receipt must:

- echo the supplied `mutation_id` and configured `ProviderId`;
- describe an outcome in the adapter's namespace;
- return the resulting playlist version when supported;
- use `PartiallyApplied` only with an exact, non-empty failure partition.

Batch limits are preconditions. Reject an oversized batch before applying any
part of it.

## 3. Optional facets

### Remote transport

Implement `RemoteTransport` only when `ProviderCaps::transport` is `Some`.
The transport's provider ID and URI scheme must exactly match its
`MusicProvider` allocation. Advertise each read/mutation independently:
playback, queue, devices, play/pause, seek, queue add, transfer, and so on.

`TransportDevice::Active` never implies a hidden transfer. Return
`ProviderError::NoActiveDevice` when no active device exists. Dispatch
transport writes through the playback-control priority lane and return only
snapshots the operation can authoritatively provide.

### Provider extras

Install `ProviderExtras` for provider-native semantics, not native protocols.
For example, expose synchronized lyrics or radio results; do not expose an
HTTP endpoint or proprietary request bus to handlers. The registry validates
facet identity and derives `ProviderCaps::extras` from the installed object,
so declaring extras without installing the facet cannot leak false
capabilities.

### Local player

`PlayerBackend` is separate from remote transport. It uses typed
`ResourceUri`s and must report the same provider ID and URI scheme as the
registry entry. Current daemon construction pairs a local player only with a
transport-capable default provider and permits one installed player. A new
multi-player topology is a separate design change, not adapter-local scope.

Backends own device registration, local playback commands, events, bounded
shutdown, and optional preload/queue/audio-counter behavior. Return
`PlayerError::Unsupported` for an individual optional backend method and
`ProviderPolicy` for provider/account restrictions. The daemon binds that
event stream to this registry entry and emits a provider-tagged, redacted
policy event; adapters must still avoid placing secrets in error text.

## 4. Configuration and authentication

Provider configuration lives under a named table:

```toml
[providers]
default = "example"

[providers.example]
type = "example"
# adapter-owned fields
```

Multiple providers require an explicit default. `spotuify-config` preserves
provider-table order and unknown fields; the adapter deserializes and
validates its own `ProviderEntry::raw_table()`. Add the adapter kind to both
the daemon factory's validation and construction match arms. Validation must
be a side-effect-free prepare step: malformed reloads cannot consume player
state, probe auth, or replace a working registry.

Authentication strategy follows the concrete adapter kind, never a provider
ID spelling or URI scheme. Keep credentials scoped to the configured
`ProviderId` under the instance-specific auth directory. Do not let a fake or
no-auth adapter read, purge, or borrow another adapter's credentials. Secrets
must not enter config diagnostics, errors, analytics, or `Debug` output.

If the adapter needs interactive auth, extend the daemon-owned auth session
state machine and expose it through the existing protocol. CLI, TUI, MCP, and
macOS clients remain render/poll clients; they do not link the vendor auth SDK
or write credential files.

## 5. Errors, retries, and timeouts

Map native failures at the adapter boundary:

- auth states to `AuthRequired`, `AuthExpired`, or `AuthRevoked`;
- throttling to `RateLimited { scope, retry_after }`;
- retryable connectivity/5xx failures to `Network`, `Transient`, or 5xx
  `Upstream`;
- policy denial to `Forbidden` or `ProviderPolicy` on the player facet;
- malformed caller input to `InvalidInput`;
- response-shape failures to `Decode`.

Do not flatten typed failures into `Provider(String)`. Retry only errors for
which `ProviderError::is_retryable()` is true, respect `retry_after`, and keep
foreground/playback/background budgets separate. Every network request,
credential operation, provider call, and shutdown path needs a bounded
timeout. Error text must be useful but bounded and must not include tokens,
headers, raw credentials, or unbounded upstream bodies.

## 6. Factory, daemon, and client wiring

Registration order:

1. Add the adapter crate with no daemon/client dependencies.
2. Add its config decode and side-effect-free validation in
   `spotuify-daemon::provider_factory`.
3. Construct one shared adapter allocation and its optional facets. Let
   `ProviderRuntime` validate identity/capability pairing.
4. Route explicit provider scope by `ProviderId` and canonical resources by
   `ResourceUri::scheme`; never infer provider ownership from a bare ID.
5. Include the provider in sync discovery. Persistence and locks remain
   provider-scoped.
6. Expose every user capability through the CLI and daemon protocol. Add the
   TUI, MCP, and macOS surfaces when the workflow is useful there.

Read/list commands must retain stable JSON output. Mutations need dry-run when
feasible and must use the normal durable operation/receipt path. Clients gate
actions from the provider catalog, but the daemon remains authoritative and
returns a clean capability error for unsupported dispatch.

## 7. Conformance and verification

Create deterministic fixtures for every declared media kind and capability,
then run the shared harness:

```rust
run_provider_conformance(&provider, &fixtures, options).await?;
if let Some(transport) = provider.capabilities().transport.as_ref() {
    run_transport_conformance(&provider, transport, &fixtures).await?;
}
```

The harness checks canonical output, paging and limits, observable library and
playlist mutations, version changes, receipt identity, and declared transport
behavior. It does not replace adapter-specific tests for native error mapping,
auth refresh/revocation, partial remote failures, extras, or a local player.

Before merge, also prove two configured instances can coexist. The current
dual-fake proof covers configuration, registry/default construction, URI and
scoped-search routing, and provider-isolated sync persistence.

```bash
scripts/cargo-nextest -p spotuify-provider-fake
scripts/cargo-nextest -p spotuify-config -E 'test(dual_fake_config)'
scripts/cargo-nextest -p spotuify-daemon -E 'test(dual_fake_config)'
scripts/cargo-nextest -p spotuify-sync -E 'test(dual_real_fake_sync)'
scripts/cargo-test --workspace
scripts/smoke.sh
```

Then run real CLI drills against an isolated development instance:

```bash
./target/release/spotuify providers list --format json
./target/release/spotuify search "known fixture" --provider <id> --format json
./target/release/spotuify sync library --provider <id> --format json
./target/release/spotuify playlist create "adapter-check" --from candidates.jsonl --provider <id> --dry-run
```

Use live provider APIs only for an explicit, bounded opt-in check. Never make
live API traffic the default smoke or conformance path.

## Review checklist

- Capability declarations match installed facets and tested behavior.
- Every output URI and source belongs to the configured provider.
- Page and batch bounds fail before I/O or mutation.
- Errors preserve type, retryability, retry delay, and provider identity.
- Auth/config reload cannot affect another provider or destroy working state.
- Sync cursors, locks, cached rows, operations, and events remain
  provider-scoped.
- CLI JSON and dry-run behavior are covered end to end.
- Transportless providers degrade cleanly; player-first behavior stays green
  for providers that do play.
