#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release

./target/release/spotuify doctor
./target/release/spotuify devices --format json
./target/release/spotuify search "luther vandross" --type track --format json

if [[ "${SPOTUIFY_LIVE_PLAYBACK:-}" == "1" ]]; then
  ./target/release/spotuify play "luther vandross"
  ./target/release/spotuify next
fi
