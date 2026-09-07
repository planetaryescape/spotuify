#[cfg(unix)]
#[test]
fn ga_live_smoke_command_traces_do_not_corrupt_redirected_json_files(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::TempDir::new()?;
    let fake_bin = temp.path().join("spotuify-fake");
    std::fs::write(
        &fake_bin,
        r#"#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "doctor" ]]; then
  if [[ "$*" == *"--format json"* ]]; then
    echo '{"daemon":{"audio_health":{"is_playing":true,"samples_advancing":true}}}'
  else
    echo "doctor ok"
  fi
elif [[ "${1:-}" == "daemon" && "${2:-}" == "restart" ]]; then
  echo restart >>"$FAKE_STATE_DIR/restarts"
  echo "restart ok"
elif [[ "${1:-}" == "daemon" && "${2:-}" == "status" ]]; then
  echo '{"running":true}'
elif [[ "${1:-}" == "status" ]]; then
  echo status >>"$FAKE_STATE_DIR/statuses"
  status_count="$(wc -l <"$FAKE_STATE_DIR/statuses" | tr -d ' ')"
  if [[ "$status_count" == "2" ]]; then
    echo '{"item":null,"is_playing":false}'
  else
    echo '{"item":{"uri":"spotify:track:ok"},"is_playing":true}'
  fi
elif [[ "${1:-}" == "devices" ]]; then
  echo '[]'
elif [[ "${1:-}" == "search" ]]; then
  echo '[]'
elif [[ "${1:-}" == "play" ]]; then
  echo '{"ok":true}'
elif [[ "${1:-}" == "next" ]]; then
  echo '{"ok":true}'
elif [[ "${1:-}" == "queue" ]]; then
  echo '{"items":[]}'
elif [[ "${1:-}" == "playlist" && "${2:-}" == "plan" ]]; then
  echo '{"description":"plan"}'
elif [[ "${1:-}" == "resolve-tracks" ]]; then
  from=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--from" ]]; then
      from="${2:?missing --from path}"
      shift 2
      continue
    fi
    shift
  done
  first_byte="$(head -c 1 "$from")"
  if [[ "$first_byte" != "{" ]]; then
    echo "plan was contaminated before JSON" >&2
    exit 64
  fi
  echo '{"uri":"spotify:track:ok"}'
elif [[ "${1:-}" == "playlist" && "${2:-}" == "create" ]]; then
  echo '{"dry_run":true}'
else
  echo "unexpected command: $*" >&2
  exit 99
fi
"#,
    )?;
    let mut permissions = std::fs::metadata(&fake_bin)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_bin, permissions)?;

    let output = std::process::Command::new("bash")
        .arg("scripts/ga-live-smoke.sh")
        .env("SPOTUIFY_BIN", &fake_bin)
        .env("SPOTUIFY_GA_LIVE_PLAYBACK", "1")
        .env("SPOTUIFY_GA_RESTART_POLL_SECS", "0")
        .env("SPOTUIFY_GA_PLAYBACK_STABILITY_SECS", "0")
        .env("FAKE_STATE_DIR", temp.path())
        .output()?;

    assert!(
        output.status.success(),
        "script should not write command traces into redirected JSON files\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("restarts"))?
            .lines()
            .count(),
        2,
        "playback smoke must restart once after playback begins"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("statuses"))?
            .lines()
            .count(),
        4,
        "playback smoke must wait for restart recovery, then recheck after the watchdog window"
    );
    Ok(())
}
