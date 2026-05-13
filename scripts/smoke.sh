#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release

if [[ -n "${SPOTUIFY_SMOKE_DIR:-}" ]]; then
  fake_root="$SPOTUIFY_SMOKE_DIR"
else
  fake_root="$(mktemp -d "${TMPDIR:-/tmp}/spotuify-smoke.XXXXXX")"
fi

fake_spotuify() {
  SPOTUIFY_FAKE_SPOTIFY=1 \
    SPOTUIFY_RUNTIME_DIR="$fake_root/runtime" \
    SPOTUIFY_SOCKET="$fake_root/runtime/daemon.sock" \
    SPOTUIFY_CACHE_DB="$fake_root/cache.sqlite" \
    SPOTUIFY_SEARCH_INDEX="$fake_root/index" \
    SPOTUIFY_ANALYTICS_DB="$fake_root/analytics.sqlite" \
    SPOTUIFY_CONFIG="$fake_root/spotuify.toml" \
    ./target/release/spotuify "$@"
}

cleanup() {
  fake_spotuify daemon stop >/dev/null 2>&1 || true
  if [[ -z "${SPOTUIFY_SMOKE_DIR:-}" ]]; then
    rm -rf "$fake_root"
  fi
}
trap cleanup EXIT

fake_spotuify doctor
fake_spotuify devices --format json
fake_spotuify search "luther vandross" --type track --format json

if [[ "${SPOTUIFY_LIVE_API:-}" == "1" ]]; then
  ./target/release/spotuify doctor
  ./target/release/spotuify devices --format json
  ./target/release/spotuify search "luther vandross" --type track --format json
fi

if [[ "${SPOTUIFY_LIVE_PLAYBACK:-}" == "1" ]]; then
  ./target/release/spotuify play "luther vandross"
  ./target/release/spotuify next
fi
