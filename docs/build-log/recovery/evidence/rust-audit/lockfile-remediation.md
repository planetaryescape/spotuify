# Rust advisory lockfile remediation

- Worker checkout: `/Users/bhekanik/code/bhekanik/.worktrees/spotuify-ci-20261026`
- Base and current `origin/main`: `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e` (fetched from HTTPS remote).
- Scope: only `Cargo.lock` changed for this task. Existing site package edits and untracked self-hosted action were preserved.
- CI source: PR 154 run 40328052864, job 121589887389, sanitized log at `../r1-rust-audit-ci.log`; its check runs `cargo deny check advisories licenses` and failed with seven RustSec findings.

## Lock changes

| Package | Before | After | RustSec finding(s) | Published crate metadata |
| --- | --- | --- | --- | --- |
| `rustls` | 0.23.45 | 0.23.46 | 2026-0346, 0347, 0348, 0349 | rust-version 1.71; published 2026-10-14; checksum `48e13bd8c0e9365c43cfa5c9e8f9ad49d3c8444926c9aac819e0e4dc503c8fdf` |
| `rustls-webpki` | 0.103.15 | 0.103.16 | 2026-0382 | rust-version 1.71; published 2026-10-20; checksum `4443b1b6d39d7c663139942cc4558262562d2c84123bb6282ef177009ca8b281` |
| `tokio` | 1.52.3 | 1.53.3 | 2026-0386 | rust-version 1.71; published 2026-10-13; checksum `ce3335fa71841cda333a58d7615b03901380ecf09d59b3296d21f8bbac0dde4e` |
| `webpki-roots` | 1.0.7 | 1.0.10 | 2026-0381 | rust-version 1.71; published 2026-10-19; checksum `1c1c8e20c8a38fc7d65abfe74eb7280e4aa667e8eec8bc84996d2874810748d9` |

These updates satisfy existing requirements: three patch updates and a compatible Tokio minor update. No `Cargo.toml` change was needed. The lockfile continues resolving compatibility package `webpki-roots 0.26.11` through fixed `webpki-roots 1.0.10`.

Primary advisory records:

- https://rustsec.org/advisories/RUSTSEC-2026-0346
- https://rustsec.org/advisories/RUSTSEC-2026-0347
- https://rustsec.org/advisories/RUSTSEC-2026-0348
- https://rustsec.org/advisories/RUSTSEC-2026-0349
- https://rustsec.org/advisories/RUSTSEC-2026-0381
- https://rustsec.org/advisories/RUSTSEC-2026-0382
- https://rustsec.org/advisories/RUSTSEC-2026-0386

Crate metadata was queried from the crates.io API on 2026-10-26. All four patched crates declare Rust 1.71; workspace `Cargo.toml` declares Rust 1.88. Tokio advisory 0386 deprecates safe `AsyncFd::{new,with_interest,try_new,try_with_interest}` constructors in favor of unsafe registration APIs starting in 1.51.6/1.53.3; `git grep -n AsyncFd origin/main -- '*.rs'` returned no project source call sites.

## Verification

- `cargo deny check advisories licenses` — exit 0, `advisories ok, licenses ok`.
- `cargo tree --locked -i rustls-webpki@0.103.16 -i tokio@1.53.3` — exit 0, graph resolved from lockfile.
- Lockfile parse confirmed exactly `rustls 0.23.46`, `rustls-webpki 0.103.16`, `tokio 1.53.3`, and `webpki-roots 0.26.11, 1.0.10`.
- `git diff --check -- Cargo.lock` — clean.
- The first ambiguous `cargo update -p webpki-roots` invocation named both locked versions and failed without changing the lock. Retried with `cargo update -p webpki-roots@1.0.7 --precise 1.0.10`.
- Initial Cargo resolver edits moved several `windows-sys` dependency references; those were restored to the base lockfile values. Final diff is limited to four fixed package entries, their checksums, and references to the roots 1.x package.

No Rust build or test ran in this worker. Root owns the integration build and full checks.

Windows MSVC portability checks also passed: `cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc` resolved 576 packages/nodes; adding `--all-features` resolved 598. The lockfile SHA-256 stayed `1b75cb154e5ed8ac3949512f4fb401ae358de02b33ea69ffddd3a884d36be423` before and after both checks. Restored Windows dependency choices satisfy the updated package requirements; Tokio retains `windows-sys 0.61.2`, matching its `0.61` requirement. Root integration has the same lockfile hash.
