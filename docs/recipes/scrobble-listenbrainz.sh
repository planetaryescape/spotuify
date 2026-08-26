#!/usr/bin/env bash
#
# Submit each qualified spotuify listen to ListenBrainz.
#
# Configure this script as `analytics.hook_command`. The daemon also invokes
# hooks for other playback events, so the event guard below is required.
#
# Required environment:
#
#   LISTENBRAINZ_TOKEN   from https://listenbrainz.org/profile/
#
# Optional environment:
#
#   LISTENBRAINZ_API     defaults to https://api.listenbrainz.org
#
# Runtime dependencies: curl, jq.

set -euo pipefail

[[ "${SPOTUIFY_EVENT:-}" == "listen-qualified" ]] || exit 0

: "${LISTENBRAINZ_TOKEN:?missing LISTENBRAINZ_TOKEN; see https://listenbrainz.org/profile/}"
: "${SPOTUIFY_URI:?missing SPOTUIFY_URI from spotuify hook}"
: "${SPOTUIFY_TRACK:?missing SPOTUIFY_TRACK from spotuify hook}"
: "${SPOTUIFY_ARTIST:?missing SPOTUIFY_ARTIST from spotuify hook}"

LISTENBRAINZ_API="${LISTENBRAINZ_API:-https://api.listenbrainz.org}"
started_at_ms="${SPOTUIFY_STARTED_AT_MS:-$(( $(date +%s) * 1000 ))}"
listened_at=$(( started_at_ms / 1000 ))

payload="$(jq -n \
  --argjson listened_at "${listened_at}" \
  --arg track_name "${SPOTUIFY_TRACK}" \
  --arg artist_name "${SPOTUIFY_ARTIST}" \
  --arg release_name "${SPOTUIFY_ALBUM:-}" \
  --arg origin_url "https://open.spotify.com/track/${SPOTUIFY_URI##*:}" \
  --argjson duration_ms "${SPOTUIFY_DURATION_MS:-0}" \
  '{
    listen_type: "single",
    payload: [{
      listened_at: $listened_at,
      track_metadata: {
        track_name: $track_name,
        artist_name: $artist_name,
        release_name: $release_name,
        additional_info: {
          duration_ms: $duration_ms,
          music_service: "spotify.com",
          origin_url: $origin_url
        }
      }
    }]
  }')"

if ! response="$(curl --silent --show-error --fail --max-time 4 \
  --request POST \
  --header "Authorization: Token ${LISTENBRAINZ_TOKEN}" \
  --header "Content-Type: application/json" \
  --data "${payload}" \
  "${LISTENBRAINZ_API}/1/submit-listens")"; then
  echo "ListenBrainz scrobble request failed" >&2
  exit 1
fi

if [[ "$(jq -r '.status // empty' <<<"${response}")" != "ok" ]]; then
  echo "ListenBrainz scrobble failed: $(jq -r '.error // "unexpected API response"' <<<"${response}")" >&2
  exit 1
fi
