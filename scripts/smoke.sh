#!/usr/bin/env bash
set -euo pipefail

if [[ "${SPOTUIFY_SMOKE_BUILD:-}" == "1" ]]; then
  case "$(uname -s)" in
    Darwin) audio_feature="portaudio-backend" ;;
    Linux) audio_feature="${SPOTUIFY_SMOKE_AUDIO_FEATURE:-alsa-backend}" ;;
    MINGW*|MSYS*|CYGWIN*) audio_feature="rodio-backend" ;;
    *) audio_feature="${SPOTUIFY_SMOKE_AUDIO_FEATURE:-rodio-backend}" ;;
  esac
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test --locked
  cargo build --locked --release --no-default-features \
    --features "embedded-playback system-integrations loopback-cpal ${audio_feature}"
fi

SPOTUIFY_BIN="${SPOTUIFY_BIN:-./target/release/spotuify}"
if [[ ! -x "$SPOTUIFY_BIN" ]]; then
  cat >&2 <<EOF
missing smoke binary: $SPOTUIFY_BIN

Build it explicitly first, or run:
  SPOTUIFY_SMOKE_BUILD=1 scripts/smoke.sh

Set SPOTUIFY_BIN=/path/to/spotuify to smoke a different binary.
EOF
  exit 2
fi

if [[ -n "${SPOTUIFY_SMOKE_DIR:-}" ]]; then
  fake_root="$SPOTUIFY_SMOKE_DIR"
else
  fake_root="$(mktemp -d "${TMPDIR:-/tmp}/spotuify-smoke.XXXXXX")"
fi

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) fake_socket="\\\\.\\pipe\\spotuify-smoke-$$-${RANDOM:-0}" ;;
  *) fake_socket="$fake_root/runtime/daemon.sock" ;;
esac

fake_spotuify() {
  SPOTUIFY_FAKE_SPOTIFY=1 \
    SPOTUIFY_INSTANCE=spotuify-smoke \
    SPOTUIFY_CLIENT_ID=fake-client-id \
    SPOTUIFY_RUNTIME_DIR="$fake_root/runtime" \
    SPOTUIFY_SOCKET="$fake_socket" \
    SPOTUIFY_DATA_DIR="$fake_root/data" \
    SPOTUIFY_CACHE_DIR="$fake_root/cache-dir" \
    SPOTUIFY_CONFIG_DIR="$fake_root/config-dir" \
    SPOTUIFY_LOG_DIR="$fake_root/logs" \
    SPOTUIFY_KEYCHAIN_SERVICE=spotuify-smoke \
    SPOTUIFY_CACHE_DB="$fake_root/cache.sqlite" \
    SPOTUIFY_SEARCH_INDEX="$fake_root/index" \
    SPOTUIFY_ANALYTICS_DB="$fake_root/analytics.sqlite" \
    SPOTUIFY_CONFIG="$fake_root/spotuify.toml" \
    "$SPOTUIFY_BIN" "$@"
}

print_smoke_logs() {
  [[ -d "$fake_root/logs" ]] || return 0
  echo "---- spotuify smoke daemon logs ----" >&2
  find "$fake_root/logs" -maxdepth 1 -type f -name 'spotuify.log*' -print \
    | sort \
    | while IFS= read -r log; do
      echo "== $log ==" >&2
      tail -n 160 "$log" >&2 || true
    done
}

cleanup() {
  status=$?
  trap - EXIT
  if [[ "$status" -ne 0 ]]; then
    print_smoke_logs
  fi
  fake_spotuify daemon stop >/dev/null 2>&1 || true
  if [[ -z "${SPOTUIFY_SMOKE_DIR:-}" ]]; then
    for _ in 1 2 3 4 5; do
      rm -rf "$fake_root" 2>/dev/null && exit "$status"
      sleep 1
    done
    rm -rf "$fake_root" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT

fake_spotuify doctor
fake_spotuify devices --format json
fake_spotuify search "luther vandross" --type track --format json

cat > "$fake_root/candidates.jsonl" <<'EOF'
{"position":1,"query":"fixture one","status":"resolved","chosen_uri":"spotify:track:never-too-much","confidence":1.0,"reason":"smoke fixture","source":"fake"}
{"position":2,"query":"fixture two","status":"resolved","chosen_uri":"spotify:track:sweet-thing","confidence":1.0,"reason":"smoke fixture","source":"fake"}
EOF
created="$(fake_spotuify playlist create "Population smoke" \
  --from "$fake_root/candidates.jsonl" --yes --format json)"
[[ "$created" == *'"ok": true'* ]]
[[ "$created" == *'"added_item_count": 2'* ]]
tracks="$(fake_spotuify playlist tracks "Population smoke" --format ids)"
[[ "$tracks" == $'spotify:track:never-too-much\nspotify:track:sweet-thing' ]]

if [[ "${SPOTUIFY_LIVE_API:-}" == "1" ]]; then
  "$SPOTUIFY_BIN" doctor
  "$SPOTUIFY_BIN" devices --format json
  "$SPOTUIFY_BIN" search "luther vandross" --type track --format json
fi

if [[ "${SPOTUIFY_LIVE_PLAYBACK:-}" == "1" ]]; then
  "$SPOTUIFY_BIN" play "luther vandross"
  "$SPOTUIFY_BIN" next
fi
