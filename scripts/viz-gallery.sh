#!/usr/bin/env bash
# Sign-off gallery for the visualizer styles.
#
# Drives the real TUI inside tmux against an isolated fake-provider daemon,
# captures a screen per visualizer style plus the overlays and themes, and
# writes one self-contained HTML page of the results. Nothing here touches the
# `spotuify` or `spotuify-dev` instances: the daemon gets its own socket, dirs,
# and config under a throwaway root.
#
# The fake provider plays no audio, so the daemon is started with
# SPOTUIFY_VIZ_SYNTH=1 — see docs/implementation/20-phase-17-audio-visualization.md.
#
# Usage: scripts/viz-gallery.sh [output-dir]
set -euo pipefail

out_dir="${1:-${TMPDIR:-/tmp}/agent-plans/cliamp-port/gallery}"
out_dir="${out_dir%/}"
raw_dir="$out_dir/raw"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPOTUIFY_BIN="${SPOTUIFY_BIN:-$repo_root/target/release/spotuify}"
if [[ ! -x "$SPOTUIFY_BIN" ]]; then
  echo "missing binary: $SPOTUIFY_BIN (cargo build --release --bin spotuify)" >&2
  exit 2
fi
SPOTUIFY_BIN="$(cd "$(dirname "$SPOTUIFY_BIN")" && pwd)/$(basename "$SPOTUIFY_BIN")"

command -v tmux >/dev/null || { echo "tmux is required" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }

root="$(mktemp -d "${TMPDIR:-/tmp}/spotuify-viz-gallery.XXXXXX")"
socket="$root/runtime/daemon.sock"
session="spotuify-viz-gallery-$$"
# Wide enough that the 12 bands each get real width, tall enough that the
# fullscreen visualizer and the pickers are not clipped.
cols=160
rows=45
# Frames the styles need to build up motion state before a capture is honest:
# the fire field has to fill, the terrain range has to scroll in.
settle=1.5

env_file="$root/env.sh"
cat >"$env_file" <<EOF
export SPOTUIFY_FAKE_SPOTIFY=1
export SPOTUIFY_CLIENT_ID=fake-client-id
export SPOTUIFY_VIZ_SYNTH=1
export SPOTUIFY_EXIT_WITH_PARENT=$$
export SPOTUIFY_RUNTIME_DIR="$root/runtime"
export SPOTUIFY_SOCKET="$socket"
export SPOTUIFY_DATA_DIR="$root/data"
export SPOTUIFY_CACHE_DIR="$root/cache-dir"
export SPOTUIFY_CONFIG_DIR="$root/config-dir"
export SPOTUIFY_LOG_DIR="$root/logs"
export SPOTUIFY_KEYCHAIN_SERVICE=spotuify-viz-gallery
export SPOTUIFY_CACHE_DB="$root/cache.sqlite"
export SPOTUIFY_SEARCH_INDEX="$root/index"
export SPOTUIFY_ANALYTICS_DB="$root/analytics.sqlite"
export SPOTUIFY_CONFIG="$root/spotuify.toml"
EOF

fake() {
  # shellcheck disable=SC1090
  ( source "$env_file"; "$SPOTUIFY_BIN" "$@" )
}

cleanup() {
  status=$?
  trap - EXIT
  tmux kill-session -t "$session" 2>/dev/null || true
  fake daemon stop >/dev/null 2>&1 || true
  if [[ "$status" -ne 0 && -d "$root/logs" ]]; then
    echo "---- daemon logs ----" >&2
    find "$root/logs" -maxdepth 1 -type f -name 'spotuify.log*' -exec tail -n 80 {} + >&2 || true
  fi
  rm -rf "$root" 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT INT TERM

say() { printf '  %s\n' "$*"; }

mkdir -p "$raw_dir"
rm -f "$raw_dir"/*.ans

echo "spotuify viz gallery"
say "binary  $SPOTUIFY_BIN"
say "root    $root"
say "output  $out_dir"

version="$(fake --version | head -n1)"
say "version $version"

# --- daemon ------------------------------------------------------------------
# The first command auto-starts the daemon; devices only fill after the fake
# provider's first poll, so wait for a non-empty list before driving anything.
say "starting isolated daemon"
for _ in $(seq 1 40); do
  if [[ "$(fake devices --format json 2>/dev/null || echo '[]')" != "[]" ]]; then
    break
  fi
  sleep 0.25
done
fake daemon status --format json >/dev/null

# Playback keeps the viz ticker awake and gives the player screen real track
# metadata to draw. `play` is search-and-play against the fake catalogue.
fake play "anything" >/dev/null 2>&1 || true
fake viz enable >/dev/null
fake theme terminal-default >/dev/null 2>&1 || true

# --- tui ---------------------------------------------------------------------
launcher="$root/tui.sh"
cat >"$launcher" <<EOF
#!/usr/bin/env bash
source "$env_file"
exec "$SPOTUIFY_BIN"
EOF
chmod +x "$launcher"

say "launching TUI in tmux (${cols}x${rows})"
tmux kill-session -t "$session" 2>/dev/null || true
tmux new-session -d -s "$session" -x "$cols" -y "$rows" "$launcher"

pane() { tmux capture-pane -e -p -t "$session"; }
keys() { tmux send-keys -t "$session" "$@"; }

# The TUI skips ratatui-image's stdin capability query in a detached tmux pane
# (nothing there answers it, and the library's reader thread would survive into
# raw mode and eat a keystroke), so there is no DSR to pre-answer here.

# The player screen is up once the transport line has rendered.
ready=0
for _ in $(seq 1 60); do
  if pane | grep -q 'spotuify'; then ready=1; break; fi
  sleep 0.25
done
[[ "$ready" == 1 ]] || { echo "TUI never reached the player screen" >&2; pane >&2; exit 1; }
sleep 1

# Assert the pane is raw before driving keys. Without this the overlay captures
# silently come out as plain player screens, which is worse than a failure.
pane_tty="$(tmux list-panes -t "$session" -F '#{pane_tty}')"
if stty -a -f "$pane_tty" 2>/dev/null | grep -Eq '(^| )icanon'; then
  echo "TUI is not in raw mode; overlay keys would be ignored" >&2
  exit 1
fi

shot() {
  local name="$1" title="$2"
  pane >"$raw_dir/$name.ans"
  printf '%s\t%s\n' "$name" "$title" >>"$raw_dir/index.tsv"
  say "captured $name"
}
: >"$raw_dir/index.tsv"

# --- every style, twice ------------------------------------------------------
# Once in the player panel, which is the surface a user actually lives with,
# and once fullscreen, which is the only size where the Braille styles are
# legible enough to sign off.
styles="$(fake viz styles --format json | python3 -c 'import json,sys; print("\n".join(s["name"] for s in json.load(sys.stdin)))')"
count="$(printf '%s\n' "$styles" | wc -l | tr -d ' ')"

capture_every_style() {
  local prefix="$1" label="$2" index=0
  say "capturing $count styles ($label)"
  while IFS= read -r style; do
    [[ -n "$style" ]] || continue
    index=$((index + 1))
    fake viz style "$style" >/dev/null
    sleep "$settle"
    shot "$prefix-$(printf '%02d' "$index")-$style" "$style ($label)"
  done <<<"$styles"
}

capture_every_style panel "player panel"

keys V
sleep 1
capture_every_style full fullscreen
keys V
sleep 0.5

# --- overlays ----------------------------------------------------------------
fake viz style flame >/dev/null
sleep "$settle"

keys C-v; sleep 0.6
keys Down Down Down Down Down Down; sleep "$settle"
shot "overlay-1-style-picker" "style picker, mid-list (ctrl+v)"
keys Escape; sleep 0.5

keys t; sleep 0.8; shot "overlay-2-theme-picker" "theme picker (t)"
keys Escape; sleep 0.5

keys E; sleep 0.8; shot "overlay-3-equalizer" "equalizer overlay (E)"
keys Escape; sleep 0.5

# --- themes ------------------------------------------------------------------
for theme in terminal-default winamp nord; do
  fake theme "$theme" >/dev/null
  sleep "$settle"
  shot "theme-$theme" "player under theme: $theme"
done
fake theme terminal-default >/dev/null

tmux kill-session -t "$session" 2>/dev/null || true

# --- html --------------------------------------------------------------------
say "rendering HTML"
GALLERY_RAW="$raw_dir" \
GALLERY_OUT="$out_dir/viz-gallery.html" \
GALLERY_VERSION="$version" \
GALLERY_SIZE="${cols}x${rows}" \
  python3 "$repo_root/scripts/ansi_to_gallery.py"

echo
echo "gallery: $out_dir/viz-gallery.html"
