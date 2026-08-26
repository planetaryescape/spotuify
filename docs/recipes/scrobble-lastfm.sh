#!/usr/bin/env bash
#
# Scrobble each qualified spotuify listen to Last.fm.
#
# Configure this script as `analytics.hook_command`. The daemon also invokes
# hooks for other playback events, so the event guard below is required.
#
# Required environment:
#
#   LASTFM_API_KEY       from https://www.last.fm/api/account/create
#   LASTFM_API_SECRET    paired secret used to sign requests
#   LASTFM_SESSION_KEY   desktop-auth session key
#
# Runtime dependencies: curl, jq, openssl.

set -euo pipefail

[[ "${SPOTUIFY_EVENT:-}" == "listen-qualified" ]] || exit 0

: "${LASTFM_API_KEY:?missing LASTFM_API_KEY}"
: "${LASTFM_API_SECRET:?missing LASTFM_API_SECRET}"
: "${LASTFM_SESSION_KEY:?missing LASTFM_SESSION_KEY}"
: "${SPOTUIFY_URI:?missing SPOTUIFY_URI from spotuify hook}"
: "${SPOTUIFY_TRACK:?missing SPOTUIFY_TRACK from spotuify hook}"
: "${SPOTUIFY_ARTIST:?missing SPOTUIFY_ARTIST from spotuify hook}"

LASTFM_API_URL="${LASTFM_API_URL:-https://ws.audioscrobbler.com/2.0/}"
duration_seconds=$(( ${SPOTUIFY_DURATION_MS:-0} / 1000 ))
started_at_ms="${SPOTUIFY_STARTED_AT_MS:-$(( $(date +%s) * 1000 ))}"
timestamp=$(( started_at_ms / 1000 ))
album="${SPOTUIFY_ALBUM:-}"

signature_input="api_key${LASTFM_API_KEY}artist${SPOTUIFY_ARTIST}duration${duration_seconds}methodtrack.scrobblesk${LASTFM_SESSION_KEY}timestamp${timestamp}track${SPOTUIFY_TRACK}"
if [[ -n "${album}" ]]; then
  signature_input="album${album}${signature_input}"
fi
api_sig="$(printf '%s' "${signature_input}${LASTFM_API_SECRET}" | openssl dgst -md5 -r | awk '{print $1}')"

curl_args=(
  --data-urlencode "method=track.scrobble"
  --data-urlencode "api_key=${LASTFM_API_KEY}"
  --data-urlencode "artist=${SPOTUIFY_ARTIST}"
  --data-urlencode "track=${SPOTUIFY_TRACK}"
  --data-urlencode "timestamp=${timestamp}"
  --data-urlencode "duration=${duration_seconds}"
  --data-urlencode "sk=${LASTFM_SESSION_KEY}"
  --data-urlencode "api_sig=${api_sig}"
  --data-urlencode "format=json"
)
if [[ -n "${album}" ]]; then
  curl_args+=(--data-urlencode "album=${album}")
fi

if ! response="$(curl --silent --show-error --fail --max-time 4 \
  --request POST "${LASTFM_API_URL}" "${curl_args[@]}")"; then
  echo "Last.fm scrobble request failed" >&2
  exit 1
fi

if [[ "$(jq -r '.error // empty' <<<"${response}")" != "" ]]; then
  echo "Last.fm scrobble failed: $(jq -r '.message // "unknown API error"' <<<"${response}")" >&2
  exit 1
fi

if [[ "$(jq -r '.scrobbles["@attr"].accepted // "0"' <<<"${response}")" != "1" ]]; then
  reason="$(jq -r '.scrobbles.scrobble.ignoredMessage["#text"] // "request was ignored"' <<<"${response}")"
  echo "Last.fm scrobble failed: ${reason}" >&2
  exit 1
fi
