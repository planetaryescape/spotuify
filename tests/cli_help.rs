use assert_cmd::Command;

fn assert_help_snapshot(name: &str, args: &[&str]) {
    let output = Command::cargo_bin("spotuify")
        .expect("spotuify binary")
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("utf8 help output");
    insta::assert_snapshot!(name, normalize_help_output(&stdout));
}

fn normalize_help_output(stdout: &str) -> String {
    let mut normalized = stdout
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");

    if stdout.ends_with('\n') {
        normalized.push('\n');
    }

    normalized
}

#[test]
fn cli_help_snapshots_cover_command_tree() {
    let cases: &[(&str, &[&str])] = &[
        ("cli_help_root", &["--help"]),
        ("cli_help_onboard", &["onboard", "--help"]),
        ("cli_help_login", &["login", "--help"]),
        ("cli_help_logout", &["logout", "--help"]),
        ("cli_help_doctor", &["doctor", "--help"]),
        ("cli_help_daemon", &["daemon", "--help"]),
        ("cli_help_daemon_start", &["daemon", "start", "--help"]),
        ("cli_help_daemon_stop", &["daemon", "stop", "--help"]),
        ("cli_help_daemon_restart", &["daemon", "restart", "--help"]),
        ("cli_help_daemon_status", &["daemon", "status", "--help"]),
        ("cli_help_status", &["status", "--help"]),
        ("cli_help_devices", &["devices", "--help"]),
        ("cli_help_search", &["search", "--help"]),
        ("cli_help_resolve_tracks", &["resolve-tracks", "--help"]),
        ("cli_help_queue", &["queue", "--help"]),
        ("cli_help_queue_add", &["queue", "add", "--help"]),
        ("cli_help_playlists", &["playlists", "--help"]),
        ("cli_help_play", &["play", "--help"]),
        ("cli_help_play_uri", &["play-uri", "--help"]),
        ("cli_help_next", &["next", "--help"]),
        ("cli_help_previous", &["previous", "--help"]),
        ("cli_help_pause", &["pause", "--help"]),
        ("cli_help_resume", &["resume", "--help"]),
        ("cli_help_toggle", &["toggle", "--help"]),
        ("cli_help_seek", &["seek", "--help"]),
        ("cli_help_volume", &["volume", "--help"]),
        ("cli_help_shuffle", &["shuffle", "--help"]),
        ("cli_help_repeat", &["repeat", "--help"]),
        ("cli_help_transfer", &["transfer", "--help"]),
        ("cli_help_playlist", &["playlist", "--help"]),
        ("cli_help_playlist_plan", &["playlist", "plan", "--help"]),
        (
            "cli_help_playlist_create",
            &["playlist", "create", "--help"],
        ),
        (
            "cli_help_playlist_tracks",
            &["playlist", "tracks", "--help"],
        ),
        ("cli_help_playlist_play", &["playlist", "play", "--help"]),
        ("cli_help_playlist_add", &["playlist", "add", "--help"]),
        (
            "cli_help_playlist_add_current",
            &["playlist", "add-current", "--help"],
        ),
        ("cli_help_library", &["library", "--help"]),
        ("cli_help_library_tracks", &["library", "tracks", "--help"]),
        ("cli_help_like", &["like", "--help"]),
        ("cli_help_save", &["save", "--help"]),
        ("cli_help_logs", &["logs", "--help"]),
        ("cli_help_logs_path", &["logs", "path", "--help"]),
        ("cli_help_logs_tail", &["logs", "tail", "--help"]),
        ("cli_help_config", &["config", "--help"]),
        ("cli_help_config_path", &["config", "path", "--help"]),
        ("cli_help_config_init", &["config", "init", "--help"]),
        ("cli_help_config_get", &["config", "get", "--help"]),
        ("cli_help_config_set", &["config", "set", "--help"]),
        ("cli_help_analytics", &["analytics", "--help"]),
        (
            "cli_help_analytics_events",
            &["analytics", "events", "--help"],
        ),
        ("cli_help_reindex", &["reindex", "--help"]),
        ("cli_help_cache", &["cache", "--help"]),
        ("cli_help_cache_status", &["cache", "status", "--help"]),
        ("cli_help_sync", &["sync", "--help"]),
    ];

    assert_eq!(cases.len(), 54);
    for (name, args) in cases {
        assert_help_snapshot(name, args);
    }
}
