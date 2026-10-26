use std::fs;
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Phase 13 (P13-A) — log output format. Plaintext is the default;
/// JSON is opt-in via `SPOTUIFY_LOG_FORMAT=json` or `--log-format json`.
/// JSON output is what agents and `spotuify logs tail --format json`
/// consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    pub fn from_env_or_default() -> Self {
        match std::env::var("SPOTUIFY_LOG_FORMAT").ok().as_deref() {
            Some("json") | Some("jsonl") => Self::Json,
            _ => Self::Text,
        }
    }
}

pub fn init() -> Result<WorkerGuard> {
    init_with_format(LogFormat::from_env_or_default())
}

pub fn init_with_format(format: LogFormat) -> Result<WorkerGuard> {
    let path = log_path()?;
    let dir = path
        .parent()
        .context("log path has no parent directory")?
        .to_path_buf();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure {}", dir.display()))?;

    // Daily rotation with a 7-file retention window. The previous
    // setup used `rolling::never` which left a single growing file —
    // a 2026-05-17 inspection found a 14 GB log dominated by stale
    // tantivy file_watcher WARNs from old test temp dirs. Daily files
    // are named `spotuify.log.YYYY-MM-DD`; older days get deleted
    // automatically.
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("spotuify.log")
        .max_log_files(7)
        .build(&dir)
        .with_context(|| format!("failed to init rolling log at {}", dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = resolve_log_filter();

    match format {
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .try_init();
        }
        LogFormat::Text => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .try_init();
        }
    }

    Ok(guard)
}

/// Resolve the log filter. `SPOTUIFY_LOG` wins; fall back to `RUST_LOG`
/// (covers installs whose service files set the standard tracing env);
/// finally a sensible default.
///
/// The default suppresses two tantivy modules at WARN level because
/// their watchers chronically log spurious `Failed to open meta file
/// ... NotFound` lines when test temp dirs disappear out from under
/// them. The errors are not actionable and used to fill the daemon
/// log at gigabyte scale.
fn resolve_log_filter() -> EnvFilter {
    EnvFilter::try_from_env("SPOTUIFY_LOG")
        .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
        .unwrap_or_else(|_| {
            EnvFilter::new(
                "spotuify=debug,info,\
                 tantivy::directory::file_watcher=error,\
                 tantivy::directory::managed_directory=error",
            )
        })
}

pub fn log_path() -> Result<PathBuf> {
    Ok(spotuify_protocol::paths::log_dir().join("spotuify.log"))
}

pub fn active_log_path() -> Result<PathBuf> {
    let base = log_path()?;
    let Some(dir) = base.parent() else {
        return Ok(base);
    };
    newest_log_file(dir).map_or(Ok(base.clone()), Ok)
}

/// Maximum log payload inspected by one tail request, plus one boundary byte.
/// Large requests fail explicitly rather than returning an incomplete tail.
const MAX_TAIL_BYTES: usize = 1024 * 1024;

/// Return the requested final lines from the active file within a 1 MiB budget.
/// A missing file and a zero-line request return empty output. If the requested
/// lines do not fit, return an error so callers cannot mistake truncation for
/// the complete diagnostic history.
pub fn read_tail(lines: usize) -> Result<String> {
    if lines == 0 {
        return Ok(String::new());
    }
    read_tail_from_path(&active_log_path()?, lines)
}

// Adapted from mxr crates/daemon/src/commands/logs.rs
// @ 3f111fb79440068663286de804909e918467ab48: pin a bounded file snapshot and
// bound record memory. Seeking fits this unfiltered, non-following tail without
// scanning all history or importing mxr's rotation/record tracking machinery.
fn read_tail_from_path(path: &Path, lines: usize) -> Result<String> {
    if lines == 0 {
        return Ok(String::new());
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    let length = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let start = length.saturating_sub(MAX_TAIL_BYTES as u64);
    // Inspect the preceding byte to distinguish a complete first line from a
    // suffix that starts in the middle of a record or UTF-8 character.
    let boundary_start = start.saturating_sub(1);
    file.seek(SeekFrom::Start(boundary_start))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(length - boundary_start)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let payload = if start == 0 {
        bytes.as_slice()
    } else if bytes.first() == Some(&b'\n') {
        &bytes[1..]
    } else {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |newline| &bytes[newline + 1..])
    };
    let contents = std::str::from_utf8(payload)
        .with_context(|| format!("log tail in {} is not valid UTF-8", path.display()))?;
    let selected = contents.lines().rev().take(lines).collect::<Vec<_>>();
    if start > 0 && selected.len() < lines {
        anyhow::bail!(
            "requested {lines} log lines exceed the {MAX_TAIL_BYTES}-byte tail limit for {}; request fewer lines or inspect the file directly",
            path.display()
        );
    }
    Ok(selected.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("spotuify.log") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        match newest {
            Some((current, _)) if modified <= current => {}
            _ => newest = Some((modified, path)),
        }
    }
    newest.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Process env is shared across parallel tests; serialise env-mutating ones.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn read_tail_handles_missing_empty_and_zero_line_requests() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        assert_eq!(read_tail_from_path(&path, 10).expect("missing file"), "");
        assert_eq!(read_tail_from_path(&path, 0).expect("zero lines"), "");
        fs::write(&path, "").expect("empty log");
        assert_eq!(read_tail_from_path(&path, 10).expect("empty file"), "");
        fs::write(&path, "ignored").expect("log");
        assert_eq!(read_tail_from_path(&path, 0).expect("zero lines"), "");
    }

    #[test]
    fn read_tail_preserves_blank_lines_crlf_and_unterminated_last_line() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        fs::write(&path, "first\r\n\r\nlast café").expect("log");
        assert_eq!(
            read_tail_from_path(&path, 1).expect("last line"),
            "last café"
        );
        assert_eq!(
            read_tail_from_path(&path, 2).expect("blank line"),
            "\nlast café"
        );
        assert_eq!(
            read_tail_from_path(&path, 10).expect("all available lines"),
            "first\n\nlast café"
        );
        fs::write(&path, "first\nlast\n").expect("terminated log");
        assert_eq!(read_tail_from_path(&path, 1).expect("last line"), "last");
    }

    #[test]
    fn read_tail_seeks_past_a_large_sparse_history() {
        use std::io::Write;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        let mut file = fs::File::create(&path).expect("log");
        file.set_len(8 * 1024 * 1024 * 1024)
            .expect("sparse history");
        file.seek(SeekFrom::End(0)).expect("seek end");
        file.write_all("\nolder\nplaying 東京\nlast café\n".as_bytes())
            .expect("write recent lines");
        assert_eq!(
            read_tail_from_path(&path, 2).expect("bounded tail"),
            "playing 東京\nlast café"
        );
    }

    #[test]
    fn read_tail_reports_an_oversized_final_line_instead_of_a_partial_record() {
        use std::io::Write;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        let mut file = fs::File::create(&path).expect("log");
        file.set_len(MAX_TAIL_BYTES as u64 + 100)
            .expect("oversized sparse line");
        file.seek(SeekFrom::End(0)).expect("seek end");
        file.write_all(b"end of oversized record")
            .expect("write suffix");
        let error = read_tail_from_path(&path, 1)
            .expect_err("one oversized record cannot fit")
            .to_string();
        assert!(error.contains("1048576-byte tail limit"), "{error}");
        assert!(error.contains("inspect the file directly"), "{error}");
    }

    #[test]
    fn read_tail_discards_a_multibyte_prefix_cut_and_keeps_complete_lines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        let mut contents = "é".repeat(MAX_TAIL_BYTES / 2);
        contents.push_str("\nnew 東京\nlast\n");
        fs::write(&path, contents).expect("log");
        assert_eq!(
            read_tail_from_path(&path, 2).expect("complete UTF-8 tail"),
            "new 東京\nlast"
        );
        assert!(read_tail_from_path(&path, 3)
            .expect_err("third line crosses budget")
            .to_string()
            .contains("tail limit"));
    }

    #[test]
    fn read_tail_keeps_a_first_line_that_starts_exactly_at_the_byte_budget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        let contents = format!("earlier\n{}", "x\n".repeat(MAX_TAIL_BYTES / 2));
        fs::write(&path, contents).expect("log");
        let tail = read_tail_from_path(&path, MAX_TAIL_BYTES / 2).expect("complete window");
        assert_eq!(tail.lines().count(), MAX_TAIL_BYTES / 2);
        assert!(read_tail_from_path(&path, MAX_TAIL_BYTES / 2 + 1).is_err());
    }

    #[test]
    fn read_tail_reports_invalid_utf8_without_lossy_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("spotuify.log");
        fs::write(&path, [0xff, b'\n']).expect("log");
        assert!(read_tail_from_path(&path, 1)
            .expect_err("invalid UTF-8")
            .to_string()
            .contains("not valid UTF-8"));
    }

    #[test]
    fn log_path_honors_spotuify_log_dir_env() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev = std::env::var_os("SPOTUIFY_LOG_DIR");
        std::env::set_var("SPOTUIFY_LOG_DIR", "/tmp/spotuify-test-logs");
        let path = log_path().expect("log_path");
        assert_eq!(path, PathBuf::from("/tmp/spotuify-test-logs/spotuify.log"));
        if let Some(v) = prev {
            std::env::set_var("SPOTUIFY_LOG_DIR", v);
        } else {
            std::env::remove_var("SPOTUIFY_LOG_DIR");
        }
    }

    #[test]
    fn active_log_path_prefers_newest_rotated_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().join("spotuify.log");
        let rotated = temp.path().join("spotuify.log.2026-05-21");
        fs::write(&base, "old").expect("write base");
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&rotated, "new").expect("write rotated");

        let active = newest_log_file(temp.path()).expect("active log");

        assert_eq!(active, rotated);
    }

    #[test]
    fn resolve_log_filter_prefers_spotuify_log_over_rust_log() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::set_var("SPOTUIFY_LOG", "spotuify=trace");
        std::env::set_var("RUST_LOG", "debug");
        let filter = resolve_log_filter();
        // EnvFilter::Display reflects the directives in the order they
        // were parsed; the spotuify-specific one must be present.
        assert!(filter.to_string().contains("spotuify"));
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    #[test]
    fn resolve_log_filter_falls_back_to_rust_log_when_spotuify_log_absent() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::remove_var("SPOTUIFY_LOG");
        std::env::set_var("RUST_LOG", "warn");
        let filter = resolve_log_filter();
        assert_eq!(filter.to_string(), "warn");
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    #[test]
    fn resolve_log_filter_falls_back_to_default_when_both_absent() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::remove_var("SPOTUIFY_LOG");
        std::env::remove_var("RUST_LOG");
        let filter = resolve_log_filter();
        let s = filter.to_string();
        assert!(s.contains("spotuify=debug") || s.contains("info"));
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    fn restore(key: &str, prev: Option<std::ffi::OsString>) {
        if let Some(v) = prev {
            std::env::set_var(key, v);
        } else {
            std::env::remove_var(key);
        }
    }
}
