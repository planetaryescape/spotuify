# Phase 0 - Stabilize Current App

## Goal

Make current Spotify auth/device/search/playback behavior reliable enough to build on.

## Deliverables

- Keep `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked`, and release build green.
- `doctor` must complete with bounded timeouts.
- `doctor` must show preferred device visibility.
- TUI input loop must never await Spotify network calls.
- Search must use valid Spotify API params.
- Playback must activate preferred device or show actionable error.

## Work items

1. Audit all keychain calls and keep timeout wrappers.
2. Audit all Spotify calls from TUI input path.
3. Add small helper command or temporary smoke command if needed for search/play verification.
4. Improve `doctor` device diagnostics:
   - preferred device configured
   - preferred device visible
   - active device
   - restricted devices
5. Improve playback error messages.

## Verification commands

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
./target/release/spotuify doctor
```

## Definition of done

The current binary can prove auth, device visibility, search validity, and playback control without freezing.
