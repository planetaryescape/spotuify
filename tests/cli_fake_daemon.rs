use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::thread::sleep;
use std::time::Duration;
use tempfile::TempDir;

struct DaemonGuard {
    socket_path: PathBuf,
    pid: Option<u64>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            let _ = StdCommand::new("kill").arg(pid.to_string()).status();
            for _ in 0..40 {
                if !self.socket_path.exists() {
                    break;
                }
                sleep(Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[test]
fn fake_daemon_cli_journey_covers_json_ids_and_mutation_receipts() {
    let temp = TempDir::new().expect("temp dir");
    let socket_path = temp.path().join("runtime/daemon.sock");
    let mut daemon = DaemonGuard {
        socket_path: socket_path.clone(),
        pid: None,
    };

    let devices = run_json(temp.path(), &["devices", "--format", "json"]);
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
    let mut command = Command::cargo_bin("spotuify").expect("spotuify binary");
    command
        .env("SPOTUIFY_FAKE_SPOTIFY", "1")
        .env("SPOTUIFY_RUNTIME_DIR", &runtime_dir)
        .env("SPOTUIFY_SOCKET", runtime_dir.join("daemon.sock"))
        .env("SPOTUIFY_CACHE_DB", root.join("cache.sqlite"))
        .env("SPOTUIFY_SEARCH_INDEX", root.join("index"))
        .env("SPOTUIFY_ANALYTICS_DB", root.join("analytics.sqlite"))
        .env("SPOTUIFY_CONFIG", root.join("spotuify.toml"));
    command
}
