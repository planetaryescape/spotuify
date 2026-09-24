# Idiomatic Rust Rubric (spotuify)

> The standard every crate and refactor in this workspace is judged against. Grounded in the
> [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html), the
> [clippy lint groups](https://doc.rust-lang.org/clippy/lints.html), and spotuify's own
> non-negotiables (AGENTS.md). Sources are authoritative (retrieval-led, not from memory).

## How to use this

1. Each refactor follows the TDD loop: **identify the behavior test → run it green → refactor → run it green again.** A behavior test must survive an implementation swap (rewrite the internals; if observable behavior is unchanged, the test still passes). If no test covers the behavior, write one first.
2. Score a crate by walking the dimensions below. Each dimension is `pass` / `partial` / `fail` with a concrete check (a tool, a grep, or a read). The lint policy in section L is machine-enforced; the rest is reviewed.
3. "Idiomatic" is not "passes every pedantic lint." Many opt-in lints are intentionally noisy (section L). Chase signal, not the 2000-warning long tail.

---

## A. Naming (RFC 430 / C-CASE, C-CONV, C-GETTER, C-ITER, C-WORD-ORDER)

- A1 `UpperCamelCase` types, `snake_case` fns/vars, `SCREAMING_SNAKE` consts. **Check:** clippy default + read.
- A2 Conversions named by cost: `as_*` (cheap borrow), `to_*` (expensive clone), `into_*` (owning). **Check:** grep `fn (as_|to_|into_)`.
- A3 Getters are `field()`, not `get_field()`. Iterator producers are `iter`/`iter_mut`/`into_iter`.
- A4 Consistent word order across the API (verb-noun vs noun-verb picked once).

## B. Interoperability (C-COMMON-TRAITS, C-CONV-TRAITS, C-GOOD-ERR, C-SEND-SYNC)

- B1 Eagerly derive `Debug` on all public types; derive `Clone/Copy/PartialEq/Eq/Hash/Default` where semantically free. **Check:** every `pub struct/enum` has `#[derive(Debug, ...)]`.
- B2 Conversions use std `From`/`TryFrom`/`AsRef`, not bespoke `from_label`/`to_x` methods that duplicate what a trait would express. **spotuify hotspot:** the `label()`+`from_label()`/`parse()` enum pairs are `Display`/`FromStr` candidates; their strings must not duplicate `#[serde(rename_all)]` as a second source of truth.
- B3 Error types impl `Error + Display + Debug`, never bare `String`. Libraries use `thiserror`; binaries use `anyhow`. Public APIs never leak `anyhow` or a third-party error type across the boundary.
- B4 Types are `Send + Sync` wherever feasible (required for `tokio::spawn`).

## C. Error handling & panics

- C1 **No `unwrap()`/`expect()`/`panic!`/`unreachable!`/`todo!`/`unimplemented!` in non-test production code**, except a documented, genuinely-infallible invariant expressed as `expect("why this cannot fail")`. **Check:** `rg '\.unwrap\(\)|\.expect\(|panic!|unreachable!|todo!|unimplemented!'` then exclude `#[cfg(test)]`/`tests/`. Enforced by `clippy::unwrap_used=warn`, `panic=warn`, `todo=warn` (workspace lints; CI allows them only under `--tests`).
- C2 Fallible IO/network/parse returns `Result`; `panic!` is reserved for unrecoverable internal-invariant violation. Validate arguments (C-VALIDATE) instead of panicking on bad input. The daemon and CLI must never crash on user input or external failure.
- C3 `?` for propagation; `anyhow` `.context()` to add breadcrumbs. Public error enums are `#[non_exhaustive]` where they may grow.
- C4 Error *kind* is carried structurally, never reconstructed by substring-matching a `Display` message. **spotuify hotspot:** `classify_error_kind`, `retry_after_seconds`, tantivy schema-mismatch matching.

## D. Async / Tokio

- D1 **Never block the runtime.** No `std::fs`, `std::thread::sleep`, blocking HTTP, or CPU-heavy work (image decode, FFT, hashing) directly inside an `async fn` on a runtime worker. Use `tokio::task::spawn_blocking` (bounded work) or `tokio::fs`. **Check:** `rg 'std::fs::|std::thread::sleep|image::load' crates/*/src` then read for `async` context.
- D2 **Every external operation has a bounded timeout** (`tokio::time::timeout` and/or a client-level timeout). Auth file IO, Spotify Web API, librespot, IPC, image fetch, LRCLIB. Non-negotiable (AGENTS.md). **Check:** each external call site is wrapped or uses a timeout-bearing client.
- D3 Do not hold a `std`/`parking_lot` lock guard across `.await` (clone/snapshot then drop the guard first). Use `tokio::sync::Mutex` only when a hold across `.await` is truly required.
- D4 Prefer bounded channels (`mpsc::channel(cap)`) over `unbounded_channel`; document any unbounded channel's backpressure rationale.
- D5 Spawned tasks are tracked (`JoinHandle`/`JoinSet`) and aborted/joined on shutdown; long loops select on a shutdown signal (`watch`/`CancellationToken`).

## E. Type safety & API design (C-NEWTYPE, C-CUSTOM-TYPE, C-BUILDER, C-STRUCT-PRIVATE)

- E1 Newtypes for distinct domain IDs (already done: `TrackId`/`AlbumId`/...). No stringly-typed dispatch where an enum fits.
- E2 Meaning conveyed by types, not bare `bool`/positional args. Functions with >5 args take a param struct (no `#[allow(too_many_arguments)]` as the resolution).
- E3 Struct fields are private with accessors where invariants matter; `pub` is the minimum needed (prefer `pub(crate)`). **spotuify hotspot:** 79 `pub` fields on the TUI `App`.
- E4 `#[must_use]` on pure query methods and builders whose result is the entire point. (Apply judiciously; `must_use_candidate` blanket noise is *not* required.)

## F. Module & function structure

- F1 No god-functions. A function over ~150 lines or with a giant match where each arm is real logic should delegate to named helpers. **spotuify hotspot:** the daemon `dispatch` (1190 lines, 62 arms).
- F2 No god-files where one screenful gives no context; split by concern into submodules. **spotuify hotspots:** `store/lib.rs`, `protocol/lib.rs`, `tui/app.rs`, `spotify/client.rs`.
- F3 Iterators/combinators over manual index loops + mutable accumulators. Prefer `map_or`/`map_or_else` over `map(..).unwrap_or(..)`; inline format args (`format!("{x}")`).
- F4 No redundant `.clone()`/`.to_owned()` (clippy `redundant_clone`); borrow (`&str`/`&[T]`/`&Path`/`impl AsRef`) where ownership is not consumed.

## G. Compile-time safety & dependencies

- G1 No `unsafe` without an audited `// SAFETY:` justification (workspace denies `unsafe_code`).
- G2 Each crate uses the workspace dependency version (`{ workspace = true }`), not a drifting local pin.
- G3 Schema-coupled SQL should be compile-time-checked (`sqlx::query!`/`query_as!` with an offline cache) rather than raw `query("...")` strings. **spotuify hotspot:** `store`.
- G4 Every `#[allow(...)]` carries a `// reason:` or uses `#[expect(..., reason="...")]`.

## H. Tests (the implementation-swap standard)

- H1 Tests verify behavior through public interfaces, not internals. They must survive a full implementation rewrite. No asserting on call counts/order, private state, or expected values computed from the implementation.
- H2 Each test covers a distinct equivalence class; includes boundary/empty/error paths, not just the happy path.
- H3 The "delete test": if the function body were replaced with `return Default`, the test must fail. No tautological asserts (`CONST == literal` with no invariant). **spotuify hotspot:** cache migration tests assert applied migration count and max version match `CACHE_VERSION`.
- H4 Critical orchestration paths have behavior tests via their seams (fakes), not just the pure helpers. **spotuify hotspot:** `sync_loop` orchestration is untested behind `SyncContext`.

## I. Documentation (C-CRATE-DOC, C-EXAMPLE, C-FAILURE)

- I1 Crate-level `//!` doc states purpose and is not stale.
- I2 Public items have a one-line doc; fallible/panicking public fns of *library* crates get `# Errors`/`# Panics`. (Internal crates: `missing_errors_doc` is allowed; do not chase it.)

---

## L. Lint policy (machine-enforced gate)

The CI gate (the before/after baseline for every change) is exactly:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings                                            # prod code
cargo clippy --workspace --tests --locked -- -D warnings -A clippy::panic -A clippy::unwrap_used  # tests
cargo test --locked            # behavior
cargo build --locked --release
scripts/smoke.sh               # fake-provider end-to-end
```

Workspace lints (`[workspace.lints]`): `unsafe_code=deny`, `unused_must_use=deny`, `clippy::{unwrap_used,panic,todo}=warn`.

**Enforce (worth fixing):** all `correctness`, `suspicious`, `perf`; most `complexity`/`style`; and these specific signal lints from the pedantic/nursery sweep: `redundant_clone`, `redundant_closure`, `map_unwrap_or`, `uninlined_format_args`, `needless_pass_by_value` (esp. `&PathBuf`→`&Path`), `match_same_arms`, `unnecessary_wraps`, `unused_async`, `semicolon_if_nothing_returned`.

**Allow (known-noisy opt-in lints; do NOT churn the codebase for these):** `missing_errors_doc`, `missing_panics_doc`, `doc_markdown` / missing-backticks, `must_use_candidate`, `missing_const_for_fn`, `module_name_repetitions`, `cast_possible_truncation`/`cast_sign_loss`/`cast_lossless`/`cast_possible_wrap` (intentional casts), `items_after_statements`, `too_long_first_doc_paragraph`, `option_if_let_else`, `similar_names`, `too_many_lines` (tracked as F1 structurally, not lint-chased). These account for the large majority of the 2014 pedantic+nursery hits and are explicitly out of scope.

---

## Per-crate scorecard template

| Dimension | core | protocol | store | search | spotify | player | sync | audio | lyrics | daemon | cli | tui | mcp | system |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| C panics | | | | | | | | | | | | | | |
| D async | | | | | | | | | | | | | | |
| F structure | | | | | | | | | | | | | | | |
| G deps/safety | | | | | | | | | | | | | | | |
| H tests | | | | | | | | | | | | | | | |

Fill `pass`/`partial`/`fail` per the checks above. Current weakest cells (from the 2026-05-23 audit): F (daemon dispatch, store/protocol/tui god-files), D (spotify auth file-lock, system cover_cache, search commit block on runtime), G3 (store raw SQL), H4 (sync orchestration untested).
