#![allow(clippy::panic, clippy::unwrap_used)]
#![cfg(not(windows))]

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::Duration;
use tempfile::TempDir;

static TEST_LOCK: Mutex<()> = Mutex::new(());

struct DaemonGuard {
    socket_path: PathBuf,
    pid: Option<u64>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            terminate_process(pid, false);
            let mut stopped = false;
            for _ in 0..40 {
                if !process_is_alive(pid) {
                    stopped = true;
                    break;
                }
                sleep(Duration::from_millis(50));
            }
            // Graceful termination didn't take in time; don't leave it running.
            if !stopped {
                terminate_process(pid, true);
            }
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn terminate_process(pid: u64, force: bool) {
    let pid = pid.to_string();
    let mut command = StdCommand::new("kill");
    if force {
        command.arg("-KILL");
    }
    let _ = command.arg(pid).status();
}

#[cfg(windows)]
fn terminate_process(pid: u64, force: bool) {
    let pid = pid.to_string();
    let mut command = StdCommand::new("taskkill");
    command.args(["/PID", &pid, "/T"]);
    if force {
        command.arg("/F");
    }
    let _ = command.status();
}

#[cfg(unix)]
fn process_is_alive(pid: u64) -> bool {
    StdCommand::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn process_is_alive(pid: u64) -> bool {
    StdCommand::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
            ),
        ])
        .status()
        .is_ok_and(|status| status.success())
}

fn serial_test() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(unix)]
#[test]
fn fake_daemon_repairs_private_runtime_and_state_permissions() {
    let _guard = serial_test();
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path: socket_path.clone(),
        pid: None,
    };

    let _ = run_json(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );

    for dir in [
        temp.path().join("runtime"),
        temp.path().join("data"),
        temp.path().join("cache-dir"),
        temp.path().join("config-dir"),
        temp.path().join("logs"),
    ] {
        let mode = std::fs::metadata(&dir)
            .unwrap_or_else(|err| panic!("metadata for {}: {err}", dir.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} should be private", dir.display());
    }

    for file in [
        socket_path,
        temp.path().join("cache.sqlite"),
        temp.path().join("analytics.sqlite"),
    ] {
        assert_becomes_private(&file);
    }
}

/// Wait for `path` to reach 0600 rather than assuming it already has.
///
/// `analytics.sqlite` is created and chmod-ed by the one-shot retention
/// pass, which `spawn_retention_loop` deliberately runs on the background
/// runtime so it does not slow startup. It therefore races the socket
/// becoming answerable: sqlite creates the file at the process umask and
/// the daemon tightens it a moment later, so a bare assertion here reads
/// 0644 whenever the runner is loaded enough. Same reason
/// `run_json_until_non_empty` exists in this file.
#[cfg(unix)]
fn assert_becomes_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut last = None;
    for _ in 0..100 {
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                if mode == 0o600 {
                    return;
                }
                last = Some(format!("{mode:o}"));
            }
            Err(err) => last = Some(err.to_string()),
        }
        sleep(Duration::from_millis(100));
    }
    panic!(
        "{} should have become private (0600), last saw {}",
        path.display(),
        last.unwrap_or_else(|| "nothing".to_string())
    );
}

#[test]
fn fake_daemon_cli_journey_covers_json_ids_and_mutation_receipts() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    let devices = run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );
    assert_eq!(devices[0]["name"].as_str(), Some("spotuify-fake"));
    assert_eq!(devices[0]["is_active"].as_bool(), Some(true));

    let search = run_json(
        temp.path(),
        &[
            "search",
            "luther vandross",
            "--type",
            "track",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        search[0]["uri"].as_str(),
        Some("spotify:track:never-too-much")
    );
    assert_eq!(search[0]["kind"].as_str(), Some("track"));

    let ids = run_stdout(
        temp.path(),
        &[
            "search",
            "luther vandross",
            "--type",
            "track",
            "--format",
            "ids",
        ],
    );
    assert_eq!(ids, "spotify:track:never-too-much\n");

    let receipt = run_json(
        temp.path(),
        &[
            "queue",
            "add",
            "spotify:track:never-too-much",
            "--format",
            "json",
        ],
    );
    assert_eq!(receipt["ok"].as_bool(), Some(true));
    assert_eq!(receipt["action"].as_str(), Some("queue"));

    // Bookmarks: pin an explicit item + clock-form position, then list,
    // annotate, and delete through the same daemon.
    let created = run_json(
        temp.path(),
        &[
            "bookmark",
            "add",
            "--uri",
            "spotify:track:never-too-much",
            "--at",
            "1:02:03",
            "--note",
            "the bassline",
            "--format",
            "json",
        ],
    );
    assert_eq!(created[0]["position_ms"].as_u64(), Some(3_723_000));
    assert_eq!(created[0]["note"].as_str(), Some("the bassline"));
    let bookmark_id = created[0]["id"].as_str().expect("bookmark id").to_string();
    let listed = run_stdout(
        temp.path(),
        &[
            "bookmark",
            "list",
            "--uri",
            "spotify:track:never-too-much",
            "--format",
            "ids",
        ],
    );
    assert_eq!(listed, format!("{bookmark_id}\n"));
    let note_ack = run_json(
        temp.path(),
        &["bookmark", "note", &bookmark_id, "--format", "json"],
    );
    assert_eq!(note_ack["ok"].as_bool(), Some(true));
    let after_clear = run_json(temp.path(), &["bookmark", "list", "--format", "json"]);
    assert!(
        after_clear[0]["note"].is_null(),
        "note cleared: {after_clear:#}"
    );
    let delete_ack = run_json(
        temp.path(),
        &["bookmark", "delete", &bookmark_id, "--format", "json"],
    );
    assert_eq!(delete_ack["ok"].as_bool(), Some(true));
    assert_eq!(
        run_stdout(temp.path(), &["bookmark", "list", "--format", "ids"]),
        ""
    );

    // Podcast speed: persisted by the daemon even though the fake provider
    // has no local player to stretch audio (`applied` stays false).
    let speed = run_json(temp.path(), &["speed", "1.5x", "--format", "json"]);
    assert_eq!(speed["podcast_speed"].as_f64(), Some(1.5));
    assert_eq!(speed["applied"].as_bool(), Some(false));
    let stepped = run_json(temp.path(), &["speed", "+", "--format", "json"]);
    assert_eq!(stepped["podcast_speed"].as_f64(), Some(1.6));
    let read_back = run_json(temp.path(), &["speed", "--format", "json"]);
    assert_eq!(read_back["podcast_speed"].as_f64(), Some(1.6));

    // Equalizer: same story — persisted by the daemon, `applied` false
    // because the fake provider has no local sink to filter.
    let rock = run_json(temp.path(), &["eq", "rock", "--format", "json"]);
    assert_eq!(rock["preset"].as_str(), Some("Rock"));
    assert_eq!(rock["applied"].as_bool(), Some(false));
    assert_eq!(rock["bands"][0].as_f64(), Some(5.0));
    let read_back = run_json(temp.path(), &["eq", "--format", "json"]);
    assert_eq!(read_back["preset"].as_str(), Some("Rock"));
    assert_eq!(read_back["bands"], rock["bands"]);
    assert_eq!(
        run_stdout(temp.path(), &["eq", "--format", "ids"]),
        "5 4 2 -1 -2 2 4 5 5 5\n"
    );

    // Editing one band keeps the other nine and drops the preset label.
    let custom = run_json(temp.path(), &["eq", "--band", "0", "6", "--format", "json"]);
    assert!(
        custom["preset"].is_null(),
        "band edit is Custom: {custom:#}"
    );
    assert_eq!(custom["bands"][0].as_f64(), Some(6.0));
    assert_eq!(custom["bands"][1].as_f64(), Some(4.0));
    // Negative gains must survive clap's hyphen handling.
    let negative = run_json(
        temp.path(),
        &["eq", "--band", "4", "-3", "--format", "json"],
    );
    assert_eq!(negative["bands"][4].as_f64(), Some(-3.0));

    let flat = run_json(temp.path(), &["eq", "--reset", "--format", "json"]);
    assert_eq!(flat["preset"].as_str(), Some("Flat"));
    assert!(
        flat["bands"]
            .as_array()
            .is_some_and(|bands| bands.iter().all(|db| db.as_f64() == Some(0.0))),
        "reset flattens: {flat:#}"
    );

    let presets = run_json(temp.path(), &["eq", "presets", "--format", "json"]);
    assert_eq!(presets.as_array().map(Vec::len), Some(16));
    assert_eq!(presets[0]["name"].as_str(), Some("Flat"));

    // Rejections are part of the contract: a scripted caller has to be able
    // to tell "you asked for something impossible" from "it worked".
    for (args, expected) in [
        (
            vec!["eq", "--band", "99", "0"],
            "band index must be 0-9, got `99`",
        ),
        (
            vec!["eq", "--band", "0", "loud"],
            "band gain must be a number",
        ),
        // Clamping 100 to 12 would report success for a request we did not
        // honour.
        (
            vec!["eq", "--band", "0", "100"],
            "band gain must be between -12 and +12 dB",
        ),
        (
            vec!["eq", "--band", "9", "-13"],
            "band gain must be between -12 and +12 dB",
        ),
        (vec!["eq", "nonsense"], "unknown eq preset `nonsense`"),
        (vec!["eq", "--reset", "rock"], "`--reset` is exclusive"),
        (vec!["eq", "presets", "--reset"], "only lists presets"),
    ] {
        let output = command(temp.path())
            .args(&args)
            .assert()
            .failure()
            .get_output()
            .clone();
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(
            stderr.contains(expected),
            "`spotuify {}` should explain itself with {expected:?}, said: {stderr}",
            args.join(" ")
        );
    }
    // A rejected command must not have touched the saved curve.
    assert_eq!(
        run_json(temp.path(), &["eq", "--format", "json"])["preset"].as_str(),
        Some("Flat")
    );
}

#[test]
fn fake_daemon_accepts_batch_ids_for_queue_and_playlist_preview() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };
    let ids_path = temp.path().join("tracks.txt");
    std::fs::write(
        &ids_path,
        "spotify:track:never-too-much\nspotify:track:sweet-thing\n",
    )
    .expect("write ids file");

    let queue = run_json(
        temp.path(),
        &[
            "queue",
            "add",
            "--ids",
            ids_path.to_str().expect("utf8 path"),
            "--format",
            "json",
        ],
    );
    assert_eq!(queue["ok"].as_bool(), Some(true));
    assert_eq!(queue["action"].as_str(), Some("queue"));
    assert_eq!(queue["requested"].as_u64(), Some(2));
    assert_eq!(queue["succeeded"].as_u64(), Some(2));
    assert_eq!(
        queue["uris"][0].as_str(),
        Some("spotify:track:never-too-much")
    );
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();

    let preview = run_json(
        temp.path(),
        &[
            "playlist",
            "add",
            "quiet-storm",
            "--ids",
            ids_path.to_str().expect("utf8 path"),
            "--dry-run",
            "--format",
            "json",
        ],
    );
    assert_eq!(preview["ok"].as_bool(), Some(true));
    assert_eq!(preview["action"].as_str(), Some("playlist-add"));
    assert_eq!(preview["dry_run"].as_bool(), Some(true));
    assert_eq!(preview["requested"].as_u64(), Some(2));
    assert_eq!(preview["succeeded"].as_u64(), Some(0));
    assert_eq!(preview["playlist"].as_str(), Some("quiet-storm"));
    assert_eq!(
        preview["playlist_uri"].as_str(),
        Some("spotify:playlist:quiet-storm")
    );
}

#[test]
fn fake_daemon_accepts_stdin_ids_for_queue() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };
    let output = command(temp.path())
        .args(["queue", "add", "--format", "ids"])
        .write_stdin("spotify:track:never-too-much\nspotify:track:sweet-thing\n")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert_eq!(
        stdout,
        "spotify:track:never-too-much\nspotify:track:sweet-thing\n"
    );
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
}

#[test]
fn playlist_batch_commit_requires_yes_outside_dry_run() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let output = command(temp.path())
        .args([
            "playlist",
            "add",
            "quiet-storm",
            "spotify:track:never-too-much",
            "spotify:track:sweet-thing",
            "--format",
            "json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("Re-run with --yes or inspect with --dry-run"),
        "unsafe batch mutation should fail closed, got {stderr:?}"
    );
}

#[test]
fn fake_daemon_routes_artist_like_to_follow_and_track_like_to_save() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    // Warm up the daemon (auto-starts on first command; devices fill after the
    // first provider poll) before capturing its pid for teardown.
    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );

    // Artist like must route to ArtistFollow. The fake provider allows Artist
    // only in follow_kinds (not save_kinds), so a LibrarySave of an artist
    // fails the mutation — a green `--wait` receipt proves the follow routing.
    let liked = run_json(
        temp.path(),
        &[
            "like",
            "spotify:artist:chaka-khan",
            "--wait",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        liked["ok"].as_bool(),
        Some(true),
        "artist like must route to follow: {liked:#}"
    );
    assert_eq!(liked["action"].as_str(), Some("like"));

    // Artist unlike must route to ArtistUnfollow (luther is pre-followed).
    let unliked = run_json(
        temp.path(),
        &[
            "unlike",
            "spotify:artist:luther-vandross",
            "--wait",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        unliked["ok"].as_bool(),
        Some(true),
        "artist unlike must route to unfollow: {unliked:#}"
    );
    assert_eq!(unliked["action"].as_str(), Some("unlike"));

    // Track like stays on the library-save path (Track is in save_kinds).
    let saved = run_json(
        temp.path(),
        &[
            "like",
            "spotify:track:never-too-much",
            "--wait",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        saved["ok"].as_bool(),
        Some(true),
        "track like must route to save: {saved:#}"
    );
    assert_eq!(saved["action"].as_str(), Some("like"));
}

#[test]
fn fake_daemon_viz_style_round_trips_through_the_daemon_and_config() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );

    let styles = run_stdout(temp.path(), &["viz", "styles", "--format", "ids"]);
    let names: Vec<&str> = styles.lines().collect();
    assert_eq!(names.len(), 28, "every style must be listed: {styles}");
    assert_eq!(names.first().copied(), Some("bars"));
    assert!(names.contains(&"classic-peak"));
    assert!(names.contains(&"wave"));

    // Default before anything is set.
    let current = run_json(temp.path(), &["viz", "style", "--format", "json"]);
    assert_eq!(current["style"].as_str(), Some("bars"));

    let set = run_json(temp.path(), &["viz", "style", "rain", "--format", "json"]);
    assert_eq!(set["style"].as_str(), Some("rain"));

    // Read back through a separate request: the daemon must have persisted it,
    // not just echoed the argument.
    let readback = run_json(temp.path(), &["viz", "style", "--format", "json"]);
    assert_eq!(readback["style"].as_str(), Some("rain"));
    let config = run_stdout(temp.path(), &["config", "get", "viz.style"]);
    assert!(
        config.contains("rain"),
        "viz.style must be persisted to config: {config}"
    );

    // `viz status` reports the same style, so one request answers "what is the
    // visualizer doing" end to end.
    let diagnostics = run_json(temp.path(), &["viz", "status", "--format", "json"]);
    assert_eq!(diagnostics["style"].as_str(), Some("rain"));

    let next = run_json(temp.path(), &["viz", "style", "next", "--format", "json"]);
    assert_eq!(next["style"].as_str(), Some("matrix"));
    let prev = run_json(temp.path(), &["viz", "style", "prev", "--format", "json"]);
    assert_eq!(prev["style"].as_str(), Some("rain"));

    // A waveform style is selectable the same way, and `viz status` sees it —
    // that is what makes the daemon start decimating waveforms into its
    // `spectrum-frame` broadcast.
    let wave = run_json(temp.path(), &["viz", "style", "wave", "--format", "json"]);
    assert_eq!(wave["style"].as_str(), Some("wave"));
    let waving = run_json(temp.path(), &["viz", "status", "--format", "json"]);
    assert_eq!(waving["style"].as_str(), Some("wave"));

    command(temp.path())
        .args(["viz", "style", "kaleidoscope"])
        .assert()
        .failure();
}

#[test]
fn fake_daemon_theme_round_trips_through_the_daemon_and_config() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );

    let listed = run_stdout(temp.path(), &["theme", "list", "--format", "ids"]);
    let names: Vec<&str> = listed.lines().collect();
    assert_eq!(
        names,
        vec![
            "terminal-default",
            "catppuccin",
            "dracula",
            "everforest",
            "gruvbox",
            "kanagawa",
            "nord",
            "rose-pine",
            "tokyo-night",
            "winamp",
        ],
        "the sentinel plus every built-in must be listed: {listed}"
    );

    // Default before anything is set: the built-in palette, no colours.
    let current = run_json(temp.path(), &["theme", "--format", "json"]);
    assert_eq!(current["name"].as_str(), Some("terminal-default"));
    assert!(current.get("accent").is_none(), "{current:#}");

    let set = run_json(temp.path(), &["theme", "winamp", "--format", "json"]);
    assert_eq!(set["name"].as_str(), Some("winamp"));
    assert_eq!(set["accent"].as_str(), Some("#00FF00"));

    // Read back through a separate request: the daemon must have persisted it,
    // not just echoed the argument.
    let readback = run_json(temp.path(), &["theme", "--format", "json"]);
    assert_eq!(readback["name"].as_str(), Some("winamp"));
    let config = run_stdout(temp.path(), &["config", "get", "tui.theme"]);
    assert!(
        config.contains("winamp"),
        "tui.theme must be persisted to config: {config}"
    );

    // `theme path` names the directory the daemon actually reads, so a user
    // can `cd` there and drop a file in without guessing.
    let themes_dir = run_stdout(temp.path(), &["theme", "path"]);
    let themes_dir = themes_dir.trim();
    assert!(themes_dir.ends_with("themes"), "{themes_dir}");

    // A user file shadows the built-in of the same name.
    std::fs::create_dir_all(themes_dir).expect("themes dir");
    std::fs::write(
        Path::new(themes_dir).join("nord.toml"),
        concat!(
            "bg = \"#010203\"\n",
            "accent = \"#ABCDEF\"\n",
            "bright_fg = \"#FFFFFF\"\n",
            "fg = \"#969696\"\n",
            "green = \"#29CE10\"\n",
            "yellow = \"#D6B521\"\n",
            "red = \"#EF3110\"\n",
        ),
    )
    .expect("write user theme");

    let listed = run_json(temp.path(), &["theme", "list", "--format", "json"]);
    // The envelope carries the applied theme as well as the catalog, so a
    // script gets both from one call.
    assert_eq!(listed["active"]["name"].as_str(), Some("winamp"));
    assert_eq!(listed["active_missing"].as_bool(), Some(false));
    let nord = listed["themes"]
        .as_array()
        .expect("themes is an array")
        .iter()
        .find(|theme| theme["name"] == "nord")
        .expect("nord is listed");
    assert_eq!(nord["source"].as_str(), Some("user"));
    assert_eq!(nord["accent"].as_str(), Some("#ABCDEF"));

    let applied = run_json(temp.path(), &["theme", "nord", "--format", "json"]);
    assert_eq!(applied["source"].as_str(), Some("user"));
    assert_eq!(applied["accent"].as_str(), Some("#ABCDEF"));

    // Deleting the applied theme's file drops it from the catalog but not
    // from the screen, and every machine format has to say so. It has to be
    // a name with no built-in behind it: delete a user `nord.toml` and the
    // built-in `nord` simply takes over, which is not the orphan case.
    let mine = Path::new(themes_dir).join("mine.toml");
    std::fs::write(
        &mine,
        concat!(
            "accent = \"#123456\"\n",
            "bright_fg = \"#FFFFFF\"\n",
            "fg = \"#969696\"\n",
            "green = \"#29CE10\"\n",
            "yellow = \"#D6B521\"\n",
            "red = \"#EF3110\"\n",
        ),
    )
    .expect("write private theme");
    run_stdout(temp.path(), &["theme", "mine"]);
    std::fs::remove_file(&mine).expect("remove private theme");

    let orphaned = run_json(temp.path(), &["theme", "list", "--format", "json"]);
    assert_eq!(orphaned["active"]["name"].as_str(), Some("mine"));
    assert_eq!(orphaned["active"]["accent"].as_str(), Some("#123456"));
    assert_eq!(orphaned["active_missing"].as_bool(), Some(true));
    assert!(
        !orphaned["themes"]
            .as_array()
            .expect("themes is an array")
            .iter()
            .any(|theme| theme["name"] == "mine"),
        "a deleted file must drop out of the pickable list: {orphaned:#}"
    );
    let csv = run_stdout(temp.path(), &["theme", "list", "--format", "csv"]);
    assert!(
        csv.lines()
            .any(|line| line.starts_with("mine,") && line.contains(",true,true,")),
        "csv must carry the applied-but-missing theme: {csv}"
    );
    let ids = run_stdout(temp.path(), &["theme", "list", "--format", "ids"]);
    assert!(
        !ids.lines().any(|line| line == "mine"),
        "`ids` lists what can be applied, and this cannot: {ids}"
    );

    // Back to a theme that exists, so the reload assertions below start from
    // a known state.
    run_stdout(temp.path(), &["theme", "nord"]);

    let failure = command(temp.path())
        .args(["theme", "kaleidoscope"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(failure.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("kaleidoscope") && stderr.contains("winamp"),
        "an unknown theme must name the alternatives: {stderr}"
    );

    // Hand-editing the key and reloading has to land too: `config set` never
    // touches the daemon, so without reload re-resolving the theme the
    // daemon would keep serving the old one to every client.
    // (`reload` needs a decodable provider entry, hence the client_id.)
    run_stdout(
        temp.path(),
        &[
            "config",
            "set",
            "providers.spotify.client_id",
            "deadbeefdeadbeefdeadbeefdeadbeef",
        ],
    );
    run_stdout(temp.path(), &["config", "set", "tui.theme", "gruvbox"]);
    assert_eq!(
        run_json(temp.path(), &["theme", "--format", "json"])["name"].as_str(),
        Some("nord"),
        "the daemon must serve its cached theme until told to reload"
    );
    run_stdout(temp.path(), &["reload"]);
    assert_eq!(
        run_json(temp.path(), &["theme", "--format", "json"])["name"].as_str(),
        Some("gruvbox"),
        "reload must re-resolve the theme from the edited config"
    );
}

#[test]
fn fake_daemon_viz_enable_disable_and_status_report_state() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();
    assert!(
        daemon.pid.is_some(),
        "fake daemon should be resident: {status:#}"
    );

    run_stdout(temp.path(), &["viz", "disable"]);
    let disabled = run_json(temp.path(), &["viz", "status", "--format", "json"]);
    assert_eq!(disabled["enabled"].as_bool(), Some(false));

    run_stdout(temp.path(), &["viz", "enable"]);
    let enabled = run_json(temp.path(), &["viz", "status", "--format", "json"]);
    assert_eq!(enabled["enabled"].as_bool(), Some(true));

    run_stdout(temp.path(), &["viz", "source", "none"]);
    let sourced = run_json(temp.path(), &["viz", "status", "--format", "json"]);
    assert_eq!(sourced["configured_source"].as_str(), Some("none"));
}

#[test]
fn fake_daemon_streams_events_to_the_events_command() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();

    // A transport mutation always emits playback-changed; the subscriber has to
    // be listening before it lands, hence the spawn-then-play ordering.
    let mut watcher = spawn_events(
        temp.path(),
        &["--kind", "playback-changed", "--once", "--timeout", "10"],
    );
    sleep(Duration::from_millis(500));
    run_stdout(temp.path(), &["play", "never too much", "--format", "json"]);
    let event = wait_for_event(&mut watcher, "playback-changed");
    assert!(
        event["received_at_ms"].as_i64().is_some(),
        "every relayed event carries its receive time: {event:#}"
    );

    // The visualizer ticker only runs while something is playing, which the
    // play above guarantees.
    let mut frames = spawn_events(
        temp.path(),
        &["--kind", "spectrum-frame", "--once", "--timeout", "20"],
    );
    let frame = wait_for_event(&mut frames, "spectrum-frame");
    assert_eq!(
        frame["bands"].as_array().map(Vec::len),
        Some(12),
        "spectrum frames carry 12 bands: {frame:#}"
    );
}

#[test]
fn fake_daemon_events_exits_cleanly_when_nothing_matches() {
    let _guard = serial_test();
    let temp = TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp.path());
    let mut daemon = DaemonGuard {
        socket_path,
        pid: None,
    };

    run_json_until_non_empty(temp.path(), &["devices", "--format", "json"]);
    let status = run_json(temp.path(), &["daemon", "status", "--format", "json"]);
    daemon.pid = status["daemon_pid"].as_u64();

    // schema-compat only fires when Spotify drops a documented key, so the fake
    // provider never emits it: the timeout is the only way out, and it has to
    // be a success with empty output even while other events stream past.
    let output = command(temp.path())
        .args(["events", "--kind", "schema-compat", "--timeout", "3"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        output.stdout.is_empty(),
        "a filtered-out stream prints nothing: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `events` is a long-running stream, so it has to run alongside the command
/// that triggers the event rather than through the blocking helpers.
fn spawn_events(root: &Path, args: &[&str]) -> std::process::Child {
    let runtime_dir = root.join("runtime");
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("spotuify"));
    command
        .arg("events")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .env("SPOTUIFY_FAKE_SPOTIFY", "1")
        .env("SPOTUIFY_EXIT_WITH_PARENT", std::process::id().to_string())
        .env("SPOTUIFY_RUNTIME_DIR", &runtime_dir)
        .env("SPOTUIFY_SOCKET", test_socket_path(root))
        .env("SPOTUIFY_DATA_DIR", root.join("data"))
        .env("SPOTUIFY_CACHE_DIR", root.join("cache-dir"))
        .env("SPOTUIFY_CONFIG_DIR", root.join("config-dir"))
        .env("SPOTUIFY_LOG_DIR", root.join("logs"))
        .env("SPOTUIFY_CACHE_DB", root.join("cache.sqlite"))
        .env("SPOTUIFY_SEARCH_INDEX", root.join("index"))
        .env("SPOTUIFY_ANALYTICS_DB", root.join("analytics.sqlite"))
        .env("SPOTUIFY_CONFIG", root.join("spotuify.toml"));
    command.spawn().expect("spawn spotuify events")
}

fn wait_for_event(child: &mut std::process::Child, kind: &str) -> Value {
    let output = child
        .wait_with_output_timeout()
        .unwrap_or_else(|| panic!("`spotuify events --kind {kind}` never returned"));
    assert!(
        output.status.success(),
        "`spotuify events --kind {kind}` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let line = stdout
        .lines()
        .next()
        .unwrap_or_else(|| panic!("no {kind} event on stdout: {stdout:?}"));
    let value: Value = serde_json::from_str(line)
        .unwrap_or_else(|err| panic!("expected JSONL from events: {err}: {line}"));
    assert_eq!(value["event"].as_str(), Some(kind));
    value
}

/// `Child::wait_with_output` has no deadline; `--timeout` bounds the process
/// itself, so this only guards against the process ignoring it.
trait WaitWithTimeout {
    fn wait_with_output_timeout(&mut self) -> Option<std::process::Output>;
}

impl WaitWithTimeout for std::process::Child {
    fn wait_with_output_timeout(&mut self) -> Option<std::process::Output> {
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            match self.try_wait().expect("poll events child") {
                Some(_) => break,
                None if std::time::Instant::now() >= deadline => {
                    let _ = self.kill();
                    return None;
                }
                None => sleep(Duration::from_millis(100)),
            }
        }
        let mut stdout = Vec::new();
        if let Some(pipe) = self.stdout.as_mut() {
            use std::io::Read as _;
            let _ = pipe.read_to_end(&mut stdout);
        }
        let status = self.wait().expect("reap events child");
        Some(std::process::Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }
}

fn run_json(root: &Path, args: &[&str]) -> Value {
    let stdout = run_stdout(root, args);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!(
            "expected JSON from `spotuify {}`: {err}\nstdout={stdout}",
            args.join(" ")
        )
    })
}

/// Like `run_json`, but for endpoints that populate asynchronously on daemon
/// cold-start (e.g. `devices`, which fills only after the first provider poll —
/// clients normally react to a `DevicesChanged` event). Retries until the JSON
/// array is non-empty, then returns the last result so the caller's assertions
/// don't race the first empty response.
fn run_json_until_non_empty(root: &Path, args: &[&str]) -> Value {
    let mut value = run_json(root, args);
    for _ in 0..50 {
        if value.as_array().is_some_and(|items| !items.is_empty()) {
            break;
        }
        sleep(Duration::from_millis(100));
        value = run_json(root, args);
    }
    value
}

fn run_stdout(root: &Path, args: &[&str]) -> String {
    let output = command(root)
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    String::from_utf8(output.stdout).expect("utf8 stdout")
}

fn command(root: &Path) -> Command {
    let runtime_dir = root.join("runtime");
    let socket_path = test_socket_path(root);
    let mut command = Command::cargo_bin("spotuify").expect("spotuify binary");
    command
        .env("SPOTUIFY_FAKE_SPOTIFY", "1")
        // Tie any auto-started daemon's lifetime to this test process so a
        // killed `cargo test`/`nextest` run can't leave an orphaned daemon.
        .env("SPOTUIFY_EXIT_WITH_PARENT", std::process::id().to_string())
        .env("SPOTUIFY_RUNTIME_DIR", &runtime_dir)
        .env("SPOTUIFY_SOCKET", socket_path)
        .env("SPOTUIFY_DATA_DIR", root.join("data"))
        .env("SPOTUIFY_CACHE_DIR", root.join("cache-dir"))
        .env("SPOTUIFY_CONFIG_DIR", root.join("config-dir"))
        .env("SPOTUIFY_LOG_DIR", root.join("logs"))
        .env("SPOTUIFY_CACHE_DB", root.join("cache.sqlite"))
        .env("SPOTUIFY_SEARCH_INDEX", root.join("index"))
        .env("SPOTUIFY_ANALYTICS_DB", root.join("analytics.sqlite"))
        .env("SPOTUIFY_CONFIG", root.join("spotuify.toml"));
    command
}

#[cfg(not(windows))]
fn test_socket_path(root: &Path) -> PathBuf {
    root.join("runtime/daemon.sock")
}

#[cfg(windows)]
fn test_socket_path(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("temp");
    let name: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    PathBuf::from(format!(
        r"\\.\pipe\spotuify-test-{}-{name}",
        std::process::id()
    ))
}
