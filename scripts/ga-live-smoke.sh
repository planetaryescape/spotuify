#!/usr/bin/env bash
set -euo pipefail

SPOTUIFY_BIN="${SPOTUIFY_BIN:-spotuify}"

usage() {
  cat <<'EOF'
Usage:
  SPOTUIFY_BIN=spotuify scripts/ga-live-smoke.sh

Local target builds default to the dev instance. To test a target build against
the real release account/config, opt into the prod instance explicitly:

  SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1 SPOTUIFY_INSTANCE=spotuify SPOTUIFY_BIN=./target/release/spotuify scripts/ga-live-smoke.sh

Default checks are live but read-only:
  doctor, daemon restart/status, devices, search, queue, playlist dry-run.

Opt-in mutation checks:
  SPOTUIFY_GA_LIVE_PLAYBACK=1   run play, queue add, next, restart/resume
  SPOTUIFY_GA_LIVE_PLAYLIST=1   create a temporary playlist and undo it

This script intentionally does not run from CI. It is a human/agent
release gate for the signed binary against a real Spotify account.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

run() {
  {
    printf '+ %q' "$SPOTUIFY_BIN"
    printf ' %q' "$@"
    printf '\n'
  } >&2
  "$SPOTUIFY_BIN" "$@"
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spotuify-ga-smoke.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

run doctor
run daemon restart
run daemon status --format json
run devices --format json
run search "luther vandross" --type track --format json
run queue --format json

plan="$tmp_dir/plan.json"
resolved="$tmp_dir/resolved.jsonl"
run playlist plan "GA smoke one upbeat soul track" --format json >"$plan"
run resolve-tracks --from "$plan" --format jsonl >"$resolved"
run playlist create "spotuify GA smoke dry-run" --from "$resolved" --dry-run --format json

if [[ "${SPOTUIFY_GA_LIVE_PLAYBACK:-}" == "1" ]]; then
  if ! command -v jq >/dev/null 2>&1; then
    echo "SPOTUIFY_GA_LIVE_PLAYBACK=1 requires jq" >&2
    exit 127
  fi
  run play "luther vandross"
  run queue add --search "never too much" --format json
  run next --format json

  before_restart="$tmp_dir/playback-before-restart.json"
  after_restart="$tmp_dir/playback-after-restart.json"
  audio_health="$tmp_dir/audio-health.json"
  run status --format json >"$before_restart"
  before_uri="$(jq -er '.item.uri | select(length > 0)' "$before_restart")"
  run daemon restart
  run status --format json >"$after_restart"
  if ! jq -e --arg uri "$before_uri" \
    '.is_playing == true and .item.uri == $uri' "$after_restart" >/dev/null; then
    echo "playback did not resume the same track after daemon restart" >&2
    exit 1
  fi

  audio_ready=false
  for _ in {1..10}; do
    run doctor --format json >"$audio_health"
    if jq -e \
      '.daemon.audio_health.is_playing == true and .daemon.audio_health.samples_advancing == true' \
      "$audio_health" >/dev/null; then
      audio_ready=true
      break
    fi
    sleep 1
  done
  if [[ "$audio_ready" != "true" ]]; then
    echo "playback state resumed after daemon restart, but audio samples did not advance" >&2
    exit 1
  fi
fi

if [[ "${SPOTUIFY_GA_LIVE_PLAYLIST:-}" == "1" ]]; then
  playlist_name="spotuify GA smoke $(date +%Y%m%d%H%M%S)"
  run playlist create "$playlist_name" --from "$resolved" --yes --format json
  run ops undo --dry-run --format json
  run ops undo --yes --format json
fi

printf 'GA live smoke completed.\n'
