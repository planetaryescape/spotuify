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

    normalized.replace("spotuify.exe", "spotuify")
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
        ("cli_help_mcp", &["mcp", "--help"]),
        ("cli_help_daemon_start", &["daemon", "start", "--help"]),
        ("cli_help_daemon_stop", &["daemon", "stop", "--help"]),
        ("cli_help_daemon_restart", &["daemon", "restart", "--help"]),
        ("cli_help_daemon_status", &["daemon", "status", "--help"]),
        ("cli_help_status", &["status", "--help"]),
        ("cli_help_devices", &["devices", "--help"]),
        ("cli_help_search", &["search", "--help"]),
        ("cli_help_search_page", &["search-page", "--help"]),
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
        ("cli_help_audio_outputs", &["audio-outputs", "--help"]),
        ("cli_help_audio_output", &["audio-output", "--help"]),
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
        (
            "cli_help_playlist_unfollow",
            &["playlist", "unfollow", "--help"],
        ),
        (
            "cli_help_playlist_set_image",
            &["playlist", "set-image", "--help"],
        ),
        ("cli_help_auth", &["auth", "--help"]),
        ("cli_help_auth_bearer", &["auth", "bearer", "--help"]),
        ("cli_help_library", &["library", "--help"]),
        ("cli_help_library_tracks", &["library", "tracks", "--help"]),
        (
            "cli_help_library_saved_tracks",
            &["library", "saved-tracks", "--help"],
        ),
        ("cli_help_library_shows", &["library", "shows", "--help"]),
        ("cli_help_show", &["show", "--help"]),
        ("cli_help_show_episodes", &["show", "episodes", "--help"]),
        ("cli_help_album", &["album", "--help"]),
        ("cli_help_album_tracks", &["album", "tracks", "--help"]),
        ("cli_help_artist", &["artist", "--help"]),
        ("cli_help_artist_albums", &["artist", "albums", "--help"]),
        (
            "cli_help_artist_followed",
            &["artist", "followed", "--help"],
        ),
        ("cli_help_artist_follow", &["artist", "follow", "--help"]),
        (
            "cli_help_artist_unfollow",
            &["artist", "unfollow", "--help"],
        ),
        ("cli_help_artist_related", &["artist", "related", "--help"]),
        ("cli_help_radio", &["radio", "--help"]),
        ("cli_help_radio_start", &["radio", "start", "--help"]),
        ("cli_help_history", &["history", "--help"]),
        ("cli_help_update", &["update", "--help"]),
        ("cli_help_episodes", &["episodes", "--help"]),
        ("cli_help_lyrics", &["lyrics", "--help"]),
        ("cli_help_lyrics_show", &["lyrics", "show", "--help"]),
        ("cli_help_lyrics_follow", &["lyrics", "follow", "--help"]),
        ("cli_help_lyrics_fetch", &["lyrics", "fetch", "--help"]),
        ("cli_help_lyrics_export", &["lyrics", "export", "--help"]),
        ("cli_help_lyrics_offset", &["lyrics", "offset", "--help"]),
        ("cli_help_viz", &["viz", "--help"]),
        ("cli_help_viz_enable", &["viz", "enable", "--help"]),
        ("cli_help_viz_disable", &["viz", "disable", "--help"]),
        ("cli_help_viz_source", &["viz", "source", "--help"]),
        ("cli_help_viz_status", &["viz", "status", "--help"]),
        ("cli_help_hooks", &["hooks", "--help"]),
        ("cli_help_hooks_test", &["hooks", "test", "--help"]),
        ("cli_help_mpris", &["mpris", "--help"]),
        ("cli_help_mpris_status", &["mpris", "status", "--help"]),
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
        ("cli_help_config_show", &["config", "show", "--help"]),
        ("cli_help_analytics", &["analytics", "--help"]),
        (
            "cli_help_analytics_events",
            &["analytics", "events", "--help"],
        ),
        ("cli_help_reindex", &["reindex", "--help"]),
        ("cli_help_cache", &["cache", "--help"]),
        ("cli_help_cache_status", &["cache", "status", "--help"]),
        ("cli_help_cache_reset", &["cache", "reset", "--help"]),
        ("cli_help_cache_repair", &["cache", "repair", "--help"]),
        ("cli_help_sync", &["sync", "--help"]),
    ];

    assert_eq!(cases.len(), 97);
    for (name, args) in cases {
        assert_help_snapshot(name, args);
    }
}
