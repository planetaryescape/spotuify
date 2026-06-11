use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use spotuify_core::{
    Device, MediaItem, Notification, Playback, Playlist, Queue, Reminder, StoredAnalyticsEvent,
    SyncedLyrics,
};
use spotuify_protocol::{
    CacheStatus, CacheSyncSummary, ListenSession, PlaylistCreateReceipt, ReindexStats,
    ResponseData, SystemDiagnostics,
};

// Re-export OutputFormat so existing `crate::output::OutputFormat`
// call sites keep compiling. The type itself lives in
// spotuify-protocol so the daemon can reference it without a cli dep.
pub use spotuify_protocol::OutputFormat;

use crate::agent_playlists::{PlaylistCreatePreview, PlaylistPlan, ResolvedTrackCandidate};

#[derive(Clone, Debug, Serialize)]
pub struct MutationOutput {
    pub ok: bool,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_name: Option<String>,
    pub requested: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub uris: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<MutationOutputError>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MutationOutputError {
    pub uri: String,
    pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MediaRefreshOutput {
    pub track_uri: String,
    pub track_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<MediaRefreshCover>,
    pub lyrics: MediaRefreshLyrics,
}

#[derive(Clone, Debug, Serialize)]
pub struct MediaRefreshCover {
    pub path: String,
    pub cache_hit: bool,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MediaRefreshLyrics {
    pub found: bool,
    pub lines: usize,
    pub offset_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatusOutput<'a> {
    pub update_available: bool,
    pub current_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_url: Option<&'a str>,
    pub upgrade: &'a spotuify_protocol::UpgradeHint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at_ms: Option<i64>,
}

/// Render an update-availability report: whether a newer release exists, the
/// versions, and the exact upgrade command/URL for this install.
#[allow(clippy::too_many_arguments)]
pub fn print_update_status(
    update_available: bool,
    current_version: &str,
    latest_version: Option<&str>,
    release_url: Option<&str>,
    upgrade: &spotuify_protocol::UpgradeHint,
    checked_at_ms: Option<i64>,
    format: OutputFormat,
) -> Result<()> {
    let output = UpdateStatusOutput {
        update_available,
        current_version,
        latest_version,
        release_url,
        upgrade,
        checked_at_ms,
    };
    match format {
        OutputFormat::Json => print_json(&output),
        OutputFormat::Jsonl => print_json_line(&output),
        OutputFormat::Ids => {
            println!("{}", latest_version.unwrap_or(current_version));
            Ok(())
        }
        OutputFormat::Csv => {
            println!("update_available,current_version,latest_version,upgrade_command,release_url");
            println!(
                "{}",
                csv_row(&[
                    &update_available.to_string(),
                    current_version,
                    latest_version.unwrap_or(""),
                    upgrade.command.as_deref().unwrap_or(""),
                    upgrade.url.as_deref().or(release_url).unwrap_or(""),
                ])
            );
            Ok(())
        }
        OutputFormat::Table => {
            if update_available {
                println!(
                    "spotuify {} is available (you have {current_version}).",
                    latest_version.unwrap_or("?")
                );
                if let Some(command) = upgrade.command.as_deref() {
                    println!("Upgrade: {command}");
                } else if let Some(url) = upgrade.url.as_deref().or(release_url) {
                    println!("Download: {url}");
                }
            } else {
                println!("spotuify {current_version} is up to date.");
            }
            Ok(())
        }
    }
}

/// Render the full config as `key -> value` pairs. JSON emits a flat object
/// (consumed by the macOS Settings editor); table/ids/csv print line forms.
pub fn print_config_values(entries: &[(String, String)], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let map: std::collections::BTreeMap<&str, &str> = entries
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            println!("{}", serde_json::to_string_pretty(&map)?);
            Ok(())
        }
        OutputFormat::Jsonl => {
            for (k, v) in entries {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({ "key": k, "value": v }))?
                );
            }
            Ok(())
        }
        OutputFormat::Csv => {
            println!("key,value");
            for (k, v) in entries {
                println!("{}", csv_row(&[k, v]));
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for (k, _) in entries {
                println!("{k}");
            }
            Ok(())
        }
        OutputFormat::Table => {
            for (k, v) in entries {
                println!("{k}\t{v}");
            }
            Ok(())
        }
    }
}

pub fn print_media_refresh(summary: &MediaRefreshOutput, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(summary),
        OutputFormat::Jsonl => print_json_line(summary),
        OutputFormat::Csv => {
            println!("track_uri,track_name,cover_path,cover_cache_hit,cover_bytes,lyrics_found,lyrics_lines,lyrics_offset_ms");
            let empty = String::new();
            let cover = summary.cover_art.as_ref();
            println!(
                "{}",
                csv_row(&[
                    &summary.track_uri,
                    &summary.track_name,
                    cover.map_or(empty.as_str(), |cover| cover.path.as_str()),
                    &cover.is_some_and(|cover| cover.cache_hit).to_string(),
                    &cover.map_or(0, |cover| cover.bytes).to_string(),
                    &summary.lyrics.found.to_string(),
                    &summary.lyrics.lines.to_string(),
                    &summary.lyrics.offset_ms.to_string(),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            println!("{}", summary.track_uri);
            Ok(())
        }
        OutputFormat::Table => {
            println!("Track: {} ({})", summary.track_name, summary.track_uri);
            match &summary.cover_art {
                Some(cover) => println!(
                    "Cover: {} ({} bytes, cache_hit={})",
                    cover.path, cover.bytes, cover.cache_hit
                ),
                None => println!("Cover: none"),
            }
            println!(
                "Lyrics: {} ({} lines, offset {} ms)",
                if summary.lyrics.found {
                    "found"
                } else {
                    "not found"
                },
                summary.lyrics.lines,
                summary.lyrics.offset_ms
            );
            Ok(())
        }
    }
}

pub fn print_playback(playback: &Playback, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(playback),
        OutputFormat::Jsonl => print_jsonl(std::slice::from_ref(playback)),
        OutputFormat::Csv => {
            println!("state,name,subtitle,device,progress_ms,uri");
            let state = if playback.is_playing {
                "playing"
            } else {
                "paused"
            };
            let empty = String::new();
            let item = playback.item.as_ref();
            let device = playback.device.as_ref().map(|device| device.name.as_str());
            println!(
                "{}",
                csv_row(&[
                    state,
                    item.map_or("", |item| item.name.as_str()),
                    item.map_or("", |item| item.subtitle.as_str()),
                    device.unwrap_or(""),
                    &playback.progress_ms.to_string(),
                    item.map_or(empty.as_str(), |item| item.uri.as_str()),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            if let Some(item) = &playback.item {
                println!("{}", item.uri);
            }
            Ok(())
        }
        OutputFormat::Table => {
            let state = if playback.is_playing {
                "playing"
            } else {
                "paused"
            };
            println!("state\t{state}");
            if let Some(item) = &playback.item {
                println!("item\t{}", item.name);
                println!("by\t{}", item.subtitle);
                println!("uri\t{}", item.uri);
            } else {
                println!("item\tnothing playing");
            }
            if let Some(device) = &playback.device {
                println!("device\t{}", device.name);
            }
            Ok(())
        }
    }
}

pub fn print_devices(devices: &[Device], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(devices),
        OutputFormat::Jsonl => print_jsonl(devices),
        OutputFormat::Csv => {
            println!("id,name,type,active,restricted,volume_percent");
            for device in devices {
                let volume = device
                    .volume_percent
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                println!(
                    "{}",
                    csv_row(&[
                        device.id.as_deref().unwrap_or(""),
                        &device.name,
                        &device.kind,
                        bool_str(device.is_active),
                        bool_str(device.is_restricted),
                        &volume,
                    ])
                );
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for device in devices {
                if let Some(id) = &device.id {
                    println!("{id}");
                }
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("ACTIVE\tTYPE\tVOLUME\tNAME\tID");
            for device in devices {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    if device.is_active { "yes" } else { "no" },
                    device.kind,
                    device
                        .volume_percent
                        .map_or_else(|| "-".to_string(), |value| value.to_string()),
                    device.name,
                    device.id.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
    }
}

pub fn print_media_items(items: &[MediaItem], format: OutputFormat) -> Result<()> {
    write_media_items(&mut io::stdout(), items, format)
}

/// Section order for an artist's discography, keyed by Spotify's `album_group`.
const DISCOGRAPHY_GROUPS: &[(&str, &str)] = &[
    ("album", "Albums"),
    ("single", "Singles & EPs"),
    ("compilation", "Compilations"),
    ("appears_on", "Appears On"),
];

/// Print an artist's discography. Machine formats (json/jsonl/ids/csv) stay
/// identical to `print_media_items` so the pipeable contract holds; the table
/// view groups by `album_group`, marks library items with `✓`, and prints a
/// count summary ("23 albums • 5 in library").
pub fn print_discography(items: &[MediaItem], format: OutputFormat) -> Result<()> {
    if !matches!(format, OutputFormat::Table) {
        return print_media_items(items, format);
    }
    let mut writer = io::stdout();
    fn render_row(writer: &mut dyn Write, item: &MediaItem) -> Result<()> {
        let mark = if item.in_library == Some(true) {
            "✓"
        } else {
            " "
        };
        let year = item
            .release_date
            .as_deref()
            .map(|date| date.get(..4).unwrap_or(date))
            .unwrap_or("");
        writeln!(writer, "  {mark} {year}\t{}\t{}", item.name, item.uri)?;
        Ok(())
    }
    for (key, label) in DISCOGRAPHY_GROUPS {
        let group: Vec<&MediaItem> = items
            .iter()
            .filter(|item| item.album_group.as_deref() == Some(*key))
            .collect();
        if group.is_empty() {
            continue;
        }
        writeln!(writer, "\n{label} ({})", group.len())?;
        for item in group {
            render_row(&mut writer, item)?;
        }
    }
    let ungrouped: Vec<&MediaItem> = items
        .iter()
        .filter(|item| {
            !DISCOGRAPHY_GROUPS
                .iter()
                .any(|(key, _)| item.album_group.as_deref() == Some(*key))
        })
        .collect();
    if !ungrouped.is_empty() {
        writeln!(writer, "\nOther ({})", ungrouped.len())?;
        for item in ungrouped {
            render_row(&mut writer, item)?;
        }
    }
    let in_library = items
        .iter()
        .filter(|item| item.in_library == Some(true))
        .count();
    writeln!(writer, "\n{} albums • {in_library} in library", items.len())?;
    Ok(())
}

/// Print listening sessions. Machine formats serialize the full session
/// objects (with their tracks); the table view shows one line per session with
/// its time span, track count, and dominant context, indented tracks beneath.
pub fn print_listen_sessions(sessions: &[ListenSession], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => return print_json(sessions),
        OutputFormat::Jsonl => {
            for session in sessions {
                print_json_line(session)?;
            }
            return Ok(());
        }
        OutputFormat::Ids => {
            for session in sessions {
                for track in &session.tracks {
                    println!("{}", track.uri);
                }
            }
            return Ok(());
        }
        OutputFormat::Csv => {
            let mut writer = io::stdout();
            writeln!(
                writer,
                "session_id,started_at_ms,ended_at_ms,track_count,context"
            )?;
            for s in sessions {
                writeln!(
                    writer,
                    "{},{},{},{},{}",
                    s.session_id,
                    s.started_at_ms,
                    s.ended_at_ms,
                    s.track_count,
                    s.context_label.as_deref().unwrap_or("")
                )?;
            }
            return Ok(());
        }
        OutputFormat::Table => {}
    }
    let mut writer = io::stdout();
    if sessions.is_empty() {
        writeln!(writer, "No listening history yet.")?;
        return Ok(());
    }
    for session in sessions {
        let label = session.context_label.as_deref().unwrap_or("Mixed");
        writeln!(
            writer,
            "\n{label} — {} track(s) [{} → {}]",
            session.track_count, session.started_at_ms, session.ended_at_ms
        )?;
        for track in &session.tracks {
            writeln!(writer, "  {}\t{}", track.name, track.subtitle)?;
        }
    }
    Ok(())
}

pub fn write_media_items<W: Write>(
    writer: &mut W,
    items: &[MediaItem],
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *writer, items)?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => {
            for item in items {
                writeln!(writer, "{}", serde_json::to_string(item)?)?;
            }
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(writer, "id,uri,type,name,subtitle,context,duration_ms")?;
            for item in items {
                writeln!(
                    writer,
                    "{}",
                    csv_row(&[
                        item.id.as_deref().unwrap_or(""),
                        &item.uri,
                        item.kind.label(),
                        &item.name,
                        &item.subtitle,
                        &item.context,
                        &item.duration_ms.to_string(),
                    ])
                )?;
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for item in items {
                writeln!(writer, "{}", item.uri)?;
            }
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "TYPE\tNAME\tSUBTITLE\tURI")?;
            for item in items {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}",
                    item.kind.label(),
                    item.name,
                    item.subtitle,
                    item.uri
                )?;
            }
            Ok(())
        }
    }
}

/// Format a Unix epoch (ms) as a local human timestamp, or `—` for invalid.
fn fmt_epoch_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string())
}

pub fn print_reminders(reminders: &[Reminder], format: OutputFormat) -> Result<()> {
    let writer = &mut io::stdout();
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *writer, reminders)?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => {
            for r in reminders {
                writeln!(writer, "{}", serde_json::to_string(r)?)?;
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for r in reminders {
                writeln!(writer, "{}", r.id)?;
            }
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(writer, "id,next_due,recurrence,state,kind,name,uri")?;
            for r in reminders {
                writeln!(
                    writer,
                    "{}",
                    csv_row(&[
                        &r.id,
                        &fmt_epoch_ms(r.next_due_at_ms),
                        r.recurrence.label(),
                        reminder_state_text(r),
                        r.media_kind.label(),
                        &r.name,
                        &r.media_uri,
                    ])
                )?;
            }
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "ID\tNEXT DUE\tREPEAT\tSTATE\tNAME")?;
            for r in reminders {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}\t{}",
                    short_id(&r.id),
                    fmt_epoch_ms(r.next_due_at_ms),
                    r.recurrence.label(),
                    reminder_state_text(r),
                    r.name,
                )?;
            }
            Ok(())
        }
    }
}

pub fn print_notifications(notifications: &[Notification], format: OutputFormat) -> Result<()> {
    let writer = &mut io::stdout();
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *writer, notifications)?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => {
            for n in notifications {
                writeln!(writer, "{}", serde_json::to_string(n)?)?;
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for n in notifications {
                writeln!(writer, "{}", n.id)?;
            }
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(writer, "id,due,state,kind,name,uri")?;
            for n in notifications {
                writeln!(
                    writer,
                    "{}",
                    csv_row(&[
                        &n.id,
                        &fmt_epoch_ms(n.due_at_ms),
                        notification_state_text(n),
                        n.media_kind.label(),
                        &n.name,
                        &n.media_uri,
                    ])
                )?;
            }
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "ID\tDUE\tSTATE\tNAME")?;
            for n in notifications {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}",
                    short_id(&n.id),
                    fmt_epoch_ms(n.due_at_ms),
                    notification_state_text(n),
                    n.name,
                )?;
            }
            Ok(())
        }
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn reminder_state_text(r: &Reminder) -> &'static str {
    use spotuify_core::ReminderState as S;
    match r.state {
        S::Active => "active",
        S::Completed => "completed",
        S::Cancelled => "cancelled",
    }
}

fn notification_state_text(n: &Notification) -> &'static str {
    use spotuify_core::NotificationState as S;
    match n.state {
        S::Unseen => "unseen",
        S::Seen => "seen",
        S::Snoozed => "snoozed",
        S::Dismissed => "dismissed",
        S::Done => "done",
    }
}

pub fn print_queue(queue: &Queue, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(queue),
        OutputFormat::Jsonl => {
            if let Some(item) = &queue.currently_playing {
                print_json_line(item)?;
            }
            print_jsonl(&queue.items)
        }
        OutputFormat::Csv => {
            println!("position,uri,type,name,subtitle,context,duration_ms");
            if let Some(item) = &queue.currently_playing {
                println!("{}", csv_media_row("now", item));
            }
            for (index, item) in queue.items.iter().enumerate() {
                println!("{}", csv_media_row(&(index + 1).to_string(), item));
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for item in &queue.items {
                println!("{}", item.uri);
            }
            Ok(())
        }
        OutputFormat::Table => {
            // Spotify ties the queue to an active Connect session.
            // When the session is gone, any rows in this payload are
            // historical and must be labelled as such.
            if !queue.session_active
                && (queue.currently_playing.is_some() || !queue.items.is_empty())
            {
                println!("# from last session — no active Spotify Connect session right now");
            } else if !queue.session_active {
                println!("# no active Spotify Connect session — queue is empty");
            }
            if let Some(item) = &queue.currently_playing {
                let label = if queue.session_active { "NOW" } else { "LAST" };
                println!("{label}\t{}\t{}", item.name, item.uri);
            }
            println!("POS\tTYPE\tNAME\tURI");
            for (index, item) in queue.items.iter().enumerate() {
                println!(
                    "{}\t{}\t{}\t{}",
                    index + 1,
                    item.kind.label(),
                    item.name,
                    item.uri
                );
            }
            Ok(())
        }
    }
}

pub fn print_playlists(playlists: &[Playlist], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(playlists),
        OutputFormat::Jsonl => print_jsonl(playlists),
        OutputFormat::Csv => {
            println!("id,name,owner,tracks_total");
            for playlist in playlists {
                println!(
                    "{}",
                    csv_row(&[
                        &playlist.id,
                        &playlist.name,
                        &playlist.owner,
                        &playlist.tracks_total.to_string(),
                    ])
                );
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for playlist in playlists {
                println!("{}", playlist.id);
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("TRACKS\tNAME\tOWNER\tID");
            for playlist in playlists {
                println!(
                    "{}\t{}\t{}\t{}",
                    playlist.tracks_total, playlist.name, playlist.owner, playlist.id
                );
            }
            Ok(())
        }
    }
}

pub fn print_playlist_plan(plan: &PlaylistPlan, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(plan),
        OutputFormat::Jsonl => print_json_line(plan),
        OutputFormat::Csv => {
            println!("title,description,target_length,mood,candidate_searches");
            println!(
                "{}",
                csv_row(&[
                    &plan.title,
                    &plan.description,
                    &plan.target_length.to_string(),
                    &plan.mood,
                    &plan.candidate_searches.join(";"),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            for query in &plan.candidate_searches {
                println!("{query}");
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("title\t{}", plan.title);
            println!("description\t{}", plan.description);
            println!("target_length\t{}", plan.target_length);
            println!("mood\t{}", plan.mood);
            println!("candidate_searches");
            for query in &plan.candidate_searches {
                println!("- {query}");
            }
            Ok(())
        }
    }
}

pub fn print_resolved_track_candidates(
    candidates: &[ResolvedTrackCandidate],
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(candidates),
        OutputFormat::Jsonl => print_jsonl(candidates),
        OutputFormat::Csv => {
            println!("position,status,query,chosen_uri,confidence,reason,source,explicit,playable");
            for candidate in candidates {
                println!(
                    "{}",
                    csv_row(&[
                        &candidate.position.to_string(),
                        candidate_status_label(candidate),
                        &candidate.query,
                        candidate.chosen_uri.as_deref().unwrap_or(""),
                        &candidate.confidence.to_string(),
                        &candidate.reason,
                        &candidate.source,
                        candidate.explicit.map_or("", bool_str),
                        candidate.playable.map_or("", bool_str),
                    ])
                );
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for candidate in candidates {
                if matches!(
                    candidate.status,
                    crate::agent_playlists::CandidateStatus::Resolved
                ) {
                    if let Some(uri) = candidate.chosen_uri.as_deref() {
                        println!("{uri}");
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("POS\tSTATUS\tQUERY\tURI\tREASON");
            for candidate in candidates {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    candidate.position,
                    candidate_status_label(candidate),
                    candidate.query,
                    candidate.chosen_uri.as_deref().unwrap_or("-"),
                    candidate.reason
                );
            }
            Ok(())
        }
    }
}

pub fn print_playlist_preview(preview: &PlaylistCreatePreview, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(preview),
        OutputFormat::Jsonl => print_json_line(preview),
        OutputFormat::Csv => {
            println!("position,uri,name,subtitle,explicit");
            for track in &preview.tracks {
                println!(
                    "{}",
                    csv_row(&[
                        &track.position.to_string(),
                        &track.uri,
                        &track.name,
                        &track.subtitle,
                        track.explicit.map_or("", bool_str),
                    ])
                );
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for track in &preview.tracks {
                println!("{}", track.uri);
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("Would create playlist `{}`", preview.name);
            println!("tracks\t{}", preview.added_item_count);
            if !preview.warnings.is_empty() {
                println!("warnings\t{}", preview.warnings.join("; "));
            }
            println!("POS\tNAME\tARTIST\tURI");
            for track in &preview.tracks {
                println!(
                    "{}\t{}\t{}\t{}",
                    track.position, track.name, track.subtitle, track.uri
                );
            }
            Ok(())
        }
    }
}

pub fn print_playlist_create_receipt(
    receipt: &PlaylistCreateReceipt,
    format: OutputFormat,
) -> Result<()> {
    write_playlist_create_receipt(&mut io::stdout(), receipt, format)
}

pub fn write_playlist_create_receipt<W: Write>(
    writer: &mut W,
    receipt: &PlaylistCreateReceipt,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *writer, receipt)?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => {
            writeln!(writer, "{}", serde_json::to_string(receipt)?)?;
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(
                writer,
                "ok,action,playlist_id,playlist_uri,name,added_item_count,message"
            )?;
            writeln!(
                writer,
                "{}",
                csv_row(&[
                    bool_str(receipt.ok),
                    &receipt.action,
                    &receipt.playlist_id,
                    &receipt.playlist_uri,
                    &receipt.name,
                    &receipt.added_item_count.to_string(),
                    &receipt.message,
                ])
            )?;
            Ok(())
        }
        OutputFormat::Ids => {
            writeln!(writer, "{}", receipt.playlist_uri)?;
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "{}", receipt.message)?;
            writeln!(writer, "playlist\t{}", receipt.playlist_uri)?;
            writeln!(writer, "added_item_count\t{}", receipt.added_item_count)?;
            Ok(())
        }
    }
}

pub fn print_basic_receipt(action: &str, message: &str, format: OutputFormat) -> Result<()> {
    write_basic_receipt(&mut io::stdout(), action, message, format)
}

pub fn write_basic_receipt<W: Write>(
    writer: &mut W,
    action: &str,
    message: &str,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(
                &mut *writer,
                &serde_json::json!({ "ok": true, "action": action, "message": message }),
            )?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => writeln!(
            writer,
            "{}",
            serde_json::to_string(
                &serde_json::json!({ "ok": true, "action": action, "message": message })
            )?
        )
        .map_err(Into::into),
        OutputFormat::Csv => {
            writeln!(writer, "ok,action,message")?;
            writeln!(writer, "{}", csv_row(&["true", action, message]))?;
            Ok(())
        }
        OutputFormat::Ids | OutputFormat::Table => {
            writeln!(writer, "{message}")?;
            Ok(())
        }
    }
}

pub fn print_item_receipt(action: &str, item: &MediaItem, format: OutputFormat) -> Result<()> {
    write_item_receipt(&mut io::stdout(), action, item, format)
}

pub fn print_mutation_output(receipt: &MutationOutput, format: OutputFormat) -> Result<()> {
    write_mutation_output(&mut io::stdout(), receipt, format)
}

pub fn write_mutation_output<W: Write>(
    writer: &mut W,
    receipt: &MutationOutput,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *writer, receipt)?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => {
            writeln!(writer, "{}", serde_json::to_string(receipt)?)?;
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(
                writer,
                "ok,action,dry_run,playlist,requested,succeeded,failed,uri,error,message"
            )?;
            if receipt.uris.is_empty() && receipt.errors.is_empty() {
                writeln!(writer, "{}", csv_mutation_row(receipt, "", ""))?;
            } else if receipt.errors.is_empty() {
                for uri in &receipt.uris {
                    writeln!(writer, "{}", csv_mutation_row(receipt, uri, ""))?;
                }
            } else {
                for error in &receipt.errors {
                    writeln!(
                        writer,
                        "{}",
                        csv_mutation_row(receipt, &error.uri, &error.error)
                    )?;
                }
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for uri in &receipt.uris {
                writeln!(writer, "{uri}")?;
            }
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "{}", receipt.message)?;
            if let Some(playlist) = &receipt.playlist_name {
                writeln!(writer, "playlist\t{playlist}")?;
            }
            writeln!(writer, "requested\t{}", receipt.requested)?;
            writeln!(writer, "succeeded\t{}", receipt.succeeded)?;
            if receipt.failed > 0 {
                writeln!(writer, "failed\t{}", receipt.failed)?;
                for error in &receipt.errors {
                    writeln!(writer, "error\t{}\t{}", error.uri, error.error)?;
                }
            }
            Ok(())
        }
    }
}

fn csv_mutation_row(receipt: &MutationOutput, uri: &str, error: &str) -> String {
    csv_row(&[
        bool_str(receipt.ok),
        &receipt.action,
        receipt.dry_run.map_or("", bool_str),
        receipt.playlist.as_deref().unwrap_or(""),
        &receipt.requested.to_string(),
        &receipt.succeeded.to_string(),
        &receipt.failed.to_string(),
        uri,
        error,
        &receipt.message,
    ])
}

pub fn write_item_receipt<W: Write>(
    writer: &mut W,
    action: &str,
    item: &MediaItem,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(
                &mut *writer,
                &serde_json::json!({ "ok": true, "action": action, "item": item }),
            )?;
            writeln!(writer)?;
            Ok(())
        }
        OutputFormat::Jsonl => writeln!(
            writer,
            "{}",
            serde_json::to_string(
                &serde_json::json!({ "ok": true, "action": action, "item": item })
            )?
        )
        .map_err(Into::into),
        OutputFormat::Csv => {
            writeln!(writer, "ok,action,id,uri,type,name,subtitle")?;
            writeln!(
                writer,
                "{}",
                csv_row(&[
                    "true",
                    action,
                    item.id.as_deref().unwrap_or(""),
                    &item.uri,
                    item.kind.label(),
                    &item.name,
                    &item.subtitle,
                ])
            )?;
            Ok(())
        }
        OutputFormat::Ids => {
            writeln!(writer, "{}", item.uri)?;
            Ok(())
        }
        OutputFormat::Table => {
            writeln!(writer, "{action}\t{}\t{}", item.name, item.uri)?;
            Ok(())
        }
    }
}

pub fn print_analytics_events(events: &[StoredAnalyticsEvent], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(events),
        OutputFormat::Jsonl => print_jsonl(events),
        OutputFormat::Csv => {
            println!("id,occurred_at_ms,source,kind,subject_uri,search_query_hash");
            for event in events {
                println!(
                    "{}",
                    csv_row(&[
                        &event.id.to_string(),
                        &event.occurred_at_ms.to_string(),
                        event.source.label(),
                        event.kind.label(),
                        event.subject_uri.as_deref().unwrap_or(""),
                        event.search_query_hash.as_deref().unwrap_or(""),
                    ])
                );
            }
            Ok(())
        }
        OutputFormat::Ids => {
            for event in events {
                println!("{}", event.id);
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("ID\tWHEN_MS\tSOURCE\tKIND\tSUBJECT");
            for event in events {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    event.id,
                    event.occurred_at_ms,
                    event.source.label(),
                    event.kind.label(),
                    event.subject_uri.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
    }
}

pub fn print_cache_status(status: &CacheStatus, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(status),
        OutputFormat::Jsonl => print_json_line(status),
        OutputFormat::Csv => {
            let freshness_json = serde_json::to_string(&status.freshness)?;
            println!("database_path,index_path,cover_cache_path,media_items,devices,playback_snapshots,queue_snapshots,queue_items,playlists,playlist_items,recent_items,library_items,search_runs,search_results,sync_events,lyrics_cache,lyrics_offsets,cover_cache_files,cover_cache_bytes,cover_cache_oldest_entry_ms,cover_cache_ttl_secs,cover_cache_max_bytes,index_documents,last_sync_at_ms,last_search_at_ms,freshness_json");
            println!(
                "{}",
                csv_row(&[
                    &status.database_path,
                    &status.index_path,
                    &status.cover_cache_path,
                    &status.media_items.to_string(),
                    &status.devices.to_string(),
                    &status.playback_snapshots.to_string(),
                    &status.queue_snapshots.to_string(),
                    &status.queue_items.to_string(),
                    &status.playlists.to_string(),
                    &status.playlist_items.to_string(),
                    &status.recent_items.to_string(),
                    &status.library_items.to_string(),
                    &status.search_runs.to_string(),
                    &status.search_results.to_string(),
                    &status.sync_events.to_string(),
                    &status.lyrics_cache.to_string(),
                    &status.lyrics_offsets.to_string(),
                    &status.cover_cache_files.to_string(),
                    &status.cover_cache_bytes.to_string(),
                    &status
                        .cover_cache_oldest_entry_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    &status.cover_cache_ttl_secs.to_string(),
                    &status.cover_cache_max_bytes.to_string(),
                    &status.index_documents.to_string(),
                    &status
                        .last_sync_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    &status
                        .last_search_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    &freshness_json,
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            println!("{}", status.database_path);
            println!("{}", status.index_path);
            if !status.cover_cache_path.is_empty() {
                println!("{}", status.cover_cache_path);
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("database\t{}", status.database_path);
            println!("index\t{}", status.index_path);
            if !status.cover_cache_path.is_empty() {
                println!("cover_cache\t{}", status.cover_cache_path);
                println!("cover_cache_files\t{}", status.cover_cache_files);
                println!("cover_cache_bytes\t{}", status.cover_cache_bytes);
                println!("cover_cache_ttl_secs\t{}", status.cover_cache_ttl_secs);
            }
            println!("media_items\t{}", status.media_items);
            println!("queue_snapshots\t{}", status.queue_snapshots);
            println!("queue_items\t{}", status.queue_items);
            println!("playlists\t{}", status.playlists);
            println!("playlist_items\t{}", status.playlist_items);
            println!("recent_items\t{}", status.recent_items);
            println!("library_items\t{}", status.library_items);
            println!("search_runs\t{}", status.search_runs);
            println!("lyrics_cache\t{}", status.lyrics_cache);
            println!("lyrics_offsets\t{}", status.lyrics_offsets);
            println!("index_documents\t{}", status.index_documents);
            println!(
                "freshness\tmedia_items fresh={} unknown={} gen={}",
                status.freshness.media_items.fresh,
                status.freshness.media_items.unknown,
                status.freshness.media_items.max_sync_generation
            );
            println!(
                "freshness\tqueue fresh_snapshots={} fresh_items={} gen={}",
                status.freshness.queue_snapshots.fresh,
                status.freshness.queue_items.fresh,
                status
                    .freshness
                    .queue_snapshots
                    .max_sync_generation
                    .max(status.freshness.queue_items.max_sync_generation)
            );
            println!(
                "freshness\tplaylists fresh={} unknown={} gen={}",
                status.freshness.playlists.fresh,
                status.freshness.playlists.unknown,
                status.freshness.playlists.max_sync_generation
            );
            Ok(())
        }
    }
}

pub fn print_system_diagnostics(
    diagnostics: &SystemDiagnostics,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(diagnostics),
        OutputFormat::Jsonl => print_json_line(diagnostics),
        OutputFormat::Csv => {
            println!("name,enabled,detail");
            println!(
                "{}",
                csv_row(&[
                    "media-controls",
                    bool_str(diagnostics.media_controls_enabled),
                    diagnostics
                        .media_controls_bus_name
                        .as_deref()
                        .unwrap_or("-"),
                ])
            );
            println!(
                "{}",
                csv_row(&[
                    "shell-hook",
                    bool_str(diagnostics.hooks_enabled),
                    diagnostics.hook_command.as_deref().unwrap_or("-"),
                ])
            );
            println!(
                "{}",
                csv_row(&[
                    "notifications",
                    bool_str(diagnostics.notifications_enabled),
                    "-",
                ])
            );
            println!(
                "{}",
                csv_row(&[
                    "discord-rpc",
                    bool_str(diagnostics.discord_enabled),
                    diagnostics.discord_application_id.as_deref().unwrap_or("-"),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            if let Some(bus_name) = diagnostics.media_controls_bus_name.as_deref() {
                println!("{bus_name}");
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!(
                "media-controls\t{}",
                bool_str(diagnostics.media_controls_enabled)
            );
            if let Some(bus_name) = diagnostics.media_controls_bus_name.as_deref() {
                println!("bus_name\t{bus_name}");
            }
            println!("shell-hook\t{}", bool_str(diagnostics.hooks_enabled));
            if let Some(command) = diagnostics.hook_command.as_deref() {
                println!("hook_command\t{command}");
            }
            println!(
                "notifications\t{}",
                bool_str(diagnostics.notifications_enabled)
            );
            println!("discord-rpc\t{}", bool_str(diagnostics.discord_enabled));
            Ok(())
        }
    }
}

pub fn export_lyrics_lrc(data: &ResponseData, output_path: Option<&Path>) -> Result<()> {
    let ResponseData::Lyrics {
        lyrics: Some(lyrics),
        ..
    } = data
    else {
        bail!("No lyrics available");
    };
    let lrc = render_lyrics_lrc(lyrics);
    if let Some(path) = output_path {
        std::fs::write(path, lrc).with_context(|| format!("write {}", path.display()))?;
    } else {
        print!("{lrc}");
    }
    Ok(())
}

fn render_lyrics_lrc(lyrics: &SyncedLyrics) -> String {
    let mut rendered = String::new();
    for line in &lyrics.lines {
        rendered.push_str(&format_lrc_timestamp(line.start_ms));
        rendered.push_str(&line.text);
        rendered.push('\n');
    }
    rendered
}

fn format_lrc_timestamp(start_ms: u64) -> String {
    let minutes = start_ms / 60_000;
    let seconds = (start_ms / 1_000) % 60;
    let centiseconds = (start_ms % 1_000) / 10;
    format!("[{minutes:02}:{seconds:02}.{centiseconds:02}]")
}

pub fn print_reindex_stats(stats: &ReindexStats, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(stats),
        OutputFormat::Jsonl => print_json_line(stats),
        OutputFormat::Csv => {
            println!("indexed,index_documents");
            println!("{},{}", stats.indexed, stats.index_documents);
            Ok(())
        }
        OutputFormat::Ids => {
            println!("{}", stats.indexed);
            Ok(())
        }
        OutputFormat::Table => {
            println!("indexed\t{}", stats.indexed);
            println!("index_documents\t{}", stats.index_documents);
            Ok(())
        }
    }
}

pub fn print_sync_summary(summary: &CacheSyncSummary, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => print_json(summary),
        OutputFormat::Jsonl => print_json_line(summary),
        OutputFormat::Csv => {
            println!("target,playback_snapshots,queue_snapshots,queue_items,devices,playlists,playlist_items,recent_items,library_items,media_items");
            println!(
                "{}",
                csv_row(&[
                    summary.target.label(),
                    &summary.playback_snapshots.to_string(),
                    &summary.queue_snapshots.to_string(),
                    &summary.queue_items.to_string(),
                    &summary.devices.to_string(),
                    &summary.playlists.to_string(),
                    &summary.playlist_items.to_string(),
                    &summary.recent_items.to_string(),
                    &summary.library_items.to_string(),
                    &summary.media_items.to_string(),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            println!("{}", summary.target.label());
            Ok(())
        }
        OutputFormat::Table => {
            println!("target\t{}", summary.target.label());
            println!("media_items\t{}", summary.media_items);
            println!("queue_snapshots\t{}", summary.queue_snapshots);
            println!("queue_items\t{}", summary.queue_items);
            println!("devices\t{}", summary.devices);
            println!("playlists\t{}", summary.playlists);
            println!("playlist_items\t{}", summary.playlist_items);
            println!("recent_items\t{}", summary.recent_items);
            println!("library_items\t{}", summary.library_items);
            Ok(())
        }
    }
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_jsonl<T: Serialize>(items: &[T]) -> Result<()> {
    for item in items {
        print_json_line(item)?;
    }
    Ok(())
}

fn print_json_line<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn csv_media_row(position: &str, item: &MediaItem) -> String {
    csv_row(&[
        position,
        &item.uri,
        item.kind.label(),
        &item.name,
        &item.subtitle,
        &item.context,
        &item.duration_ms.to_string(),
    ])
}

fn csv_row(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| csv_value(value))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_value(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn bool_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn candidate_status_label(candidate: &ResolvedTrackCandidate) -> &'static str {
    match candidate.status {
        crate::agent_playlists::CandidateStatus::Resolved => "resolved",
        crate::agent_playlists::CandidateStatus::Duplicate => "duplicate",
        crate::agent_playlists::CandidateStatus::Unresolved => "unresolved",
    }
}

/// Render any `ResponseData` shape to stdout in the requested format.
///
/// Dispatches to the existing typed `print_*` helpers when the variant
/// has a dedicated renderer; falls back to pretty-printed JSON for the
/// new Phase 10 / Phase 12 shapes so the CLI ships immediately and the
/// table renderers can land incrementally without breaking the surface.
pub fn print_response_data(
    data: &spotuify_protocol::ResponseData,
    format: OutputFormat,
) -> Result<()> {
    use spotuify_protocol::ResponseData as D;
    match data {
        D::Pong => println!("pong"),
        D::Shutdown => println!("shutdown requested"),
        D::Logs { lines } => {
            for line in lines {
                println!("{line}");
            }
        }
        // Existing typed renderers:
        D::Playback { playback } => return print_playback(playback, format),
        D::Devices { devices } => return print_devices(devices, format),
        D::SearchResults { items } | D::MediaItems { items } => {
            return print_media_items(items, format)
        }
        D::ListenSessions { sessions } => return print_listen_sessions(sessions, format),
        D::SearchStarted { query, version } => {
            // Ack for streaming-search clients; CLI never uses
            // SearchStream/SearchPage today, but render something
            // sensible in case a future caller emits this.
            println!("search started: query={query} version={version}");
        }
        D::CacheStatus { status } => return print_cache_status(status, format),
        D::Reindex { stats } => return print_reindex_stats(stats, format),
        D::Sync { summary } => return print_sync_summary(summary, format),
        D::Queue { queue } => return print_queue(queue, format),
        D::ClientSeed { .. } => return render_json_or_summary(format, data, |_| {}),
        D::Playlists { playlists } => return print_playlists(playlists, format),
        D::Image { bytes } => {
            print!("<image {} bytes>", bytes.len());
        }
        D::CoverArt {
            path,
            cache_hit,
            bytes,
            ..
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                return render_json_or_summary(format, data, |_| {})
            }
            OutputFormat::Csv => {
                println!("path,cache_hit,bytes");
                println!(
                    "{}",
                    csv_row(&[path, &cache_hit.to_string(), &bytes.to_string()])
                );
            }
            OutputFormat::Ids => println!("{path}"),
            OutputFormat::Table => {
                println!("path\t{path}");
                println!("cache_hit\t{cache_hit}");
                println!("bytes\t{bytes}");
            }
        },
        D::Mutation { receipt } => {
            return print_basic_receipt(&receipt.action, &receipt.message, format);
        }
        D::PlaylistCreate { receipt } => {
            return print_playlist_create_receipt(receipt, format);
        }
        D::Lyrics { lyrics, offset_ms } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                return render_json_or_summary(format, data, |_| {})
            }
            OutputFormat::Csv => {
                println!("start_ms,text,is_rtl");
                if let Some(lyrics) = lyrics {
                    for line in &lyrics.lines {
                        println!(
                            "{}",
                            csv_row(&[
                                &line.start_ms.to_string(),
                                &line.text,
                                &line.is_rtl.to_string(),
                            ])
                        );
                    }
                }
            }
            OutputFormat::Ids => {
                if let Some(lyrics) = lyrics {
                    println!("{}", lyrics.track_uri);
                }
            }
            OutputFormat::Table => {
                if let Some(lyrics) = lyrics {
                    println!("provider\t{}", lyrics.provider.label());
                    println!("synced\t{}", lyrics.synced);
                    println!("offset_ms\t{offset_ms}");
                    for line in &lyrics.lines {
                        println!("{}\t{}", line.start_ms, line.text);
                    }
                } else {
                    println!("No lyrics available");
                }
            }
        },
        D::LyricsOffset {
            track_uri,
            offset_ms,
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                return render_json_or_summary(format, data, |_| {})
            }
            OutputFormat::Csv => {
                println!("track_uri,offset_ms");
                println!("{}", csv_row(&[track_uri, &offset_ms.to_string()]));
            }
            OutputFormat::Ids => println!("{track_uri}"),
            OutputFormat::Table => {
                println!("track\t{track_uri}");
                println!("offset_ms\t{offset_ms}");
            }
        },
        D::DaemonStatus { status } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                let json = serde_json::to_string_pretty(status)?;
                println!("{json}");
            }
            _ => {
                println!(
                    "daemon: {}; socket={}",
                    if status.running { "running" } else { "stopped" },
                    status.socket_path,
                );
            }
        },
        D::DoctorReport { report } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                let json = serde_json::to_string_pretty(report)?;
                println!("{json}");
            }
            _ => {
                println!(
                    "doctor: {}; findings={}",
                    report.health_class.as_str(),
                    report.findings.len()
                );
                if let Some(terminal) = terminal_diagnostics() {
                    println!("{terminal}");
                }
            }
        },
        // Phase 10 / Phase 12 — minimal JSON / one-line summaries.
        // Typed table renderers can land in a follow-up; the JSON
        // surface is the long-term contract per blueprint anyway.
        D::AnalyticsTop { entries } => render_json_or_summary(format, entries, |e| {
            for row in e.iter() {
                println!(
                    "{:>4}× {:<40} {:<30} {}ms audible",
                    row.qualified_count, row.name, row.subtitle, row.total_audible_ms,
                );
            }
        })?,
        D::AnalyticsHabits { buckets } => render_json_or_summary(format, buckets, |b| {
            for row in b.iter() {
                println!(
                    "[{:?}] {} → {:.1} min · {} tracks · {} sessions",
                    row.bucket,
                    row.bucket_start_ms,
                    row.listening_minutes,
                    row.unique_tracks,
                    row.sessions,
                );
            }
        })?,
        D::AnalyticsSearch { entries } => render_json_or_summary(format, entries, |e| {
            for row in e.iter() {
                println!(
                    "{} · {} results · {}",
                    row.occurred_at_ms,
                    row.result_count,
                    row.query.as_deref().unwrap_or("<redacted>"),
                );
            }
        })?,
        D::AnalyticsRediscovery { candidates } => {
            render_json_or_summary(format, candidates, |c| {
                for row in c.iter() {
                    println!(
                        "{} ({}× qualified, {}d ago) — {} · {}",
                        row.track_uri,
                        row.qualified_count,
                        row.days_since_last_listen,
                        row.name,
                        row.subtitle,
                    );
                }
            })?
        }
        D::AnalyticsRebuildReport { report } => render_json_or_summary(format, report, |r| {
            println!(
                "Rebuilt {} events → {} listen_facts ({} qualified) in {}ms",
                r.events_processed, r.listen_facts_emitted, r.qualified_listens, r.elapsed_ms,
            );
        })?,
        D::AnalyticsPruneReport {
            rows_pruned,
            dry_run,
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "rows_pruned": rows_pruned, "dry_run": dry_run
                    }))?
                );
            }
            _ => {
                if *dry_run {
                    println!("dry-run: would prune {rows_pruned} rows (use --apply to commit)");
                } else {
                    println!("pruned {rows_pruned} rows");
                }
            }
        },
        D::Operations { ops } => render_json_or_summary(format, ops, |ops| {
            for op in ops.iter() {
                println!(
                    "{}  {:<18} {:<10} {:<8} {}",
                    op.operation_id,
                    op.kind.label(),
                    op.status.label(),
                    op.source.label(),
                    op.subject_uris.first().map_or("-", String::as_str),
                );
            }
        })?,
        D::OperationDetail { op, diff } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "op": op, "diff": diff,
                    }))?
                );
            }
            _ => {
                println!("Operation {}", op.operation_id);
                println!("  kind:        {}", op.kind.label());
                println!("  status:      {}", op.status.label());
                println!("  source:      {}", op.source.label());
                println!("  occurred_at: {}", op.occurred_at_ms);
                if let Some(finished) = op.finished_at_ms {
                    println!("  finished_at: {finished}");
                }
                println!("  reversible:  {}", op.reversible);
                if let Some(requester) = op.requester.as_deref() {
                    println!("  requester:   {requester}");
                }
                if !op.subject_uris.is_empty() {
                    println!("  subjects:");
                    for uri in &op.subject_uris {
                        println!("    - {uri}");
                    }
                }
                if let Some(receipt_id) = op.receipt_id {
                    println!("  receipt:     {receipt_id}");
                }
                if let Some(subject_op) = op.subject_op_id {
                    println!("  source op:   {subject_op}");
                }
                if let Some(undone) = op.undone_by_op_id {
                    println!("  undone by:   {undone}");
                }
                if let Some(redone) = op.redone_by_op_id {
                    println!("  redone by:   {redone}");
                }
                if let Some(err) = op.error_message.as_deref() {
                    println!("  error:       {err}");
                }
                // --diff: render the reversal plan and pre-state in a
                // human-skim format so an operator can answer "what
                // exactly would undo do?" without parsing JSON.
                if let Some(d) = diff {
                    println!("  undo plan:   {d}");
                }
                if diff.is_some() {
                    if let Some(plan) = op.reversal_plan.as_ref() {
                        if let Ok(plan_json) = serde_json::to_string_pretty(plan) {
                            println!("  plan:\n{}", indent(&plan_json, 4));
                        }
                    }
                    if let Some(pre) = op.pre_state.as_ref() {
                        if let Ok(pre_json) = serde_json::to_string_pretty(pre) {
                            println!("  pre_state:\n{}", indent(&pre_json, 4));
                        }
                    }
                }
            }
        },
        D::Ack { message } => {
            println!("{message}");
        }
        D::WebApiToken { .. } => {
            // Internal: bearer minting for CLI-direct clients. Never
            // rendered as user output (it would print a secret).
            println!("ok");
        }
        D::SearchCachePruned {
            pruned_runs,
            pruned_results,
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "pruned_runs": pruned_runs,
                        "pruned_results": pruned_results,
                    }))?
                );
            }
            _ => println!("Pruned {pruned_runs} search run(s)"),
        },
        D::VizStatus { diagnostics } => render_json_or_summary(format, diagnostics, |d| {
            println!("enabled\t{}", d.enabled);
            println!("configured\t{}", d.configured_source.as_str());
            println!("active\t{:?}", d.active_source);
            println!("playing\t{}", d.playing);
            println!("target_fps\t{}", d.target_fps);
            if let Some(backend) = d.backend_kind {
                println!("backend\t{}", backend.label());
            }
            if let Some(age_ms) = d.last_frame_age_ms {
                println!("last_frame_age_ms\t{age_ms}");
            }
            if let Some(device) = d.loopback_device_name.as_deref() {
                println!("loopback_device\t{device}");
            }
            if let Some(hint) = d.hint.as_deref() {
                println!("hint\t{hint}");
            }
        })?,
        D::OperationUndoResult {
            undo_op_id,
            succeeded,
            skipped,
            errors,
            preview,
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "undo_op_id": undo_op_id,
                        "succeeded": succeeded,
                        "skipped": skipped,
                        "errors": errors,
                        "preview": preview,
                    }))?
                );
            }
            _ => {
                // Dry-run: the daemon sends one "would undo …" line per
                // inspected op. Print those instead of the bare counts,
                // which read like nothing happened.
                if !preview.is_empty() {
                    for line in preview {
                        println!("{line}");
                    }
                    println!("dry-run: nothing executed; rerun with --yes to apply");
                } else {
                    println!(
                        "undo {}: {} succeeded, {} skipped, {} error(s)",
                        undo_op_id,
                        succeeded,
                        skipped,
                        errors.len(),
                    );
                }
                for err in errors {
                    println!("  ! {err}");
                }
            }
        },
        D::Reminders { reminders } => return print_reminders(reminders, format),
        D::Notifications { notifications } => return print_notifications(notifications, format),
        D::ReminderCreated { reminder } => {
            return print_reminders(std::slice::from_ref(reminder), format)
        }
        D::UpdateStatus {
            update_available,
            current_version,
            latest_version,
            release_url,
            upgrade,
            checked_at_ms,
        } => {
            return print_update_status(
                *update_available,
                current_version,
                latest_version.as_deref(),
                release_url.as_deref(),
                upgrade,
                *checked_at_ms,
                format,
            )
        }
    }
    Ok(())
}

/// Indent every line of `text` by `spaces` spaces. Used for the
/// `ops show --diff` plan/pre_state pretty-printing.
fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_json_or_summary<T: serde::Serialize>(
    format: OutputFormat,
    payload: T,
    summary: impl FnOnce(&T),
) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        _ => summary(&payload),
    }
    Ok(())
}

/// Best-effort terminal + cover-art-protocol summary for `doctor`. The
/// daemon can't see the caller's terminal, so this is computed
/// client-side. Only emitted to an interactive TTY (never into a pipe or
/// the JSON surface) and derived purely from env vars, so it writes no
/// terminal-query escape sequences. Mirrors the protocol the TUI's
/// `ratatui_image` picker would pick, as a heuristic.
fn terminal_diagnostics() -> Option<String> {
    use std::io::IsTerminal;
    if !io::stdout().is_terminal() {
        return None;
    }
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let term = env("TERM").unwrap_or_else(|| "unknown".to_string());
    let term_program = env("TERM_PROGRAM");
    let colorterm = env("COLORTERM");

    let protocol = if env("KITTY_WINDOW_ID").is_some() || term.contains("kitty") {
        "kitty graphics"
    } else if matches!(term_program.as_deref(), Some("iTerm.app" | "WezTerm")) {
        "iterm2 inline images"
    } else if term.contains("sixel") || env("MLTERM").is_some() {
        "sixel"
    } else {
        "unicode half-blocks (text fallback)"
    };
    let truecolor = colorterm
        .as_deref()
        .is_some_and(|c| c.contains("truecolor") || c.contains("24bit"));

    let program = term_program
        .map(|p| format!(" TERM_PROGRAM={p}"))
        .unwrap_or_default();
    Some(format!(
        "terminal: TERM={term}{program}; cover-art protocol: {protocol}; \
         truecolor: {}",
        if truecolor { "yes" } else { "no/unknown" }
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        render_lyrics_lrc, write_basic_receipt, write_item_receipt, write_media_items,
        write_mutation_output, write_playlist_create_receipt, MutationOutput, OutputFormat,
    };
    use spotuify_core::{LyricLine, LyricsProvider, MediaItem, MediaKind, SyncedLyrics};
    use spotuify_protocol::PlaylistCreateReceipt;

    fn utf8(out: Vec<u8>) -> String {
        String::from_utf8(out).expect("output should be valid UTF-8")
    }

    fn json_value(out: &[u8]) -> serde_json::Value {
        serde_json::from_slice(out).expect("output should be valid JSON")
    }

    fn json_line(line: &str) -> serde_json::Value {
        serde_json::from_str(line).expect("line should be valid JSON")
    }

    #[test]
    fn csv_media_output_is_pipeable_and_escapes_commas_and_quotes() {
        let items = vec![MediaItem {
            id: Some("track-1".to_string()),
            uri: "spotify:track:track-1".to_string(),
            name: "Hello, \"Friend\"".to_string(),
            subtitle: "Artist, Featured".to_string(),
            context: "Album".to_string(),
            duration_ms: 123_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("local".to_string()),
            freshness: Some("fresh".to_string()),
            explicit: None,
            is_playable: None,
            ..Default::default()
        }];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Csv).expect("CSV output should write");

        assert_eq!(
            utf8(out),
            "id,uri,type,name,subtitle,context,duration_ms\ntrack-1,spotify:track:track-1,track,\"Hello, \"\"Friend\"\"\",\"Artist, Featured\",Album,123000\n"
        );
    }

    #[test]
    fn json_media_output_has_stable_machine_fields() {
        let items = vec![media_item("track-1", "Never Too Much")];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Json).expect("JSON output should write");

        let value = json_value(&out);
        let first = &value[0];
        assert_eq!(first["id"], "track-1");
        assert_eq!(first["uri"], "spotify:track:track-1");
        assert_eq!(first["name"], "Never Too Much");
        assert_eq!(first["kind"], "track");
        assert_eq!(first["duration_ms"], 180000);
    }

    #[test]
    fn jsonl_media_output_is_one_json_object_per_line() {
        let items = vec![
            media_item("track-1", "Never Too Much"),
            media_item("track-2", "Sweet Thing"),
        ];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Jsonl)
            .expect("JSONL output should write");

        let output = utf8(out);
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(json_line(lines[0])["uri"], "spotify:track:track-1");
        assert_eq!(json_line(lines[1])["uri"], "spotify:track:track-2");
    }

    #[test]
    fn ids_media_output_is_uri_per_line_without_headers() {
        let items = vec![
            media_item("track-1", "Never Too Much"),
            media_item("track-2", "Sweet Thing"),
        ];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Ids).expect("IDs output should write");

        assert_eq!(utf8(out), "spotify:track:track-1\nspotify:track:track-2\n");
    }

    #[test]
    fn json_receipt_output_has_stable_shape() {
        let mut out = Vec::new();

        write_basic_receipt(&mut out, "pause", "Paused", OutputFormat::Json)
            .expect("receipt output should write");

        assert_eq!(
            utf8(out),
            "{\n  \"action\": \"pause\",\n  \"message\": \"Paused\",\n  \"ok\": true\n}\n"
        );
    }

    #[test]
    fn ids_item_receipt_outputs_only_uri() {
        let item = MediaItem {
            id: Some("track-1".to_string()),
            uri: "spotify:track:track-1".to_string(),
            name: "Track".to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 1,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        };
        let mut out = Vec::new();

        write_item_receipt(&mut out, "play", &item, OutputFormat::Ids)
            .expect("item receipt output should write");

        assert_eq!(utf8(out), "spotify:track:track-1\n");
    }

    #[test]
    fn playlist_create_receipt_json_includes_playlist_uri_and_added_count() {
        let receipt = PlaylistCreateReceipt {
            ok: true,
            action: "playlist-create".to_string(),
            playlist_id: "playlist-1".to_string(),
            playlist_uri: "spotify:playlist:playlist-1".to_string(),
            name: "Exile".to_string(),
            added_item_count: 2,
            message: "Created playlist `Exile` with 2 item(s)".to_string(),
        };
        let mut out = Vec::new();

        write_playlist_create_receipt(&mut out, &receipt, OutputFormat::Json)
            .expect("playlist create receipt should write");

        let value = json_value(&out);
        assert_eq!(value["playlist_uri"], "spotify:playlist:playlist-1");
        assert_eq!(value["added_item_count"], 2);
        assert_eq!(value["action"], "playlist-create");
    }

    #[test]
    fn dry_run_mutation_output_json_includes_counts_and_uris() {
        let receipt = MutationOutput {
            ok: true,
            action: "playlist-add".to_string(),
            dry_run: Some(true),
            playlist: Some("quiet-storm".to_string()),
            playlist_name: Some("Quiet Storm".to_string()),
            requested: 2,
            succeeded: 0,
            failed: 0,
            uris: vec!["spotify:track:1".to_string(), "spotify:track:2".to_string()],
            errors: Vec::new(),
            message: "Would add 2 item(s) to Quiet Storm".to_string(),
        };
        let mut out = Vec::new();

        write_mutation_output(&mut out, &receipt, OutputFormat::Json)
            .expect("mutation output should write");

        let value = json_value(&out);
        assert_eq!(value["action"], "playlist-add");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["requested"], 2);
        assert_eq!(value["uris"][1], "spotify:track:2");
    }

    #[test]
    fn lyrics_lrc_export_uses_centisecond_timestamps() {
        let lyrics = SyncedLyrics {
            provider: LyricsProvider::Lrclib,
            track_uri: "spotify:track:abc".to_string(),
            lines: vec![
                LyricLine {
                    start_ms: 1_230,
                    text: "first".to_string(),
                    is_rtl: false,
                },
                LyricLine {
                    start_ms: 61_999,
                    text: "second".to_string(),
                    is_rtl: false,
                },
            ],
            fetched_at_ms: 9,
            synced: true,
            language: None,
            source_url: None,
        };

        assert_eq!(
            render_lyrics_lrc(&lyrics),
            "[00:01.23]first\n[01:01.99]second\n"
        );
    }

    fn media_item(id: &str, name: &str) -> MediaItem {
        MediaItem {
            id: Some(id.to_string()),
            uri: format!("spotify:track:{id}"),
            name: name.to_string(),
            subtitle: "Luther Vandross".to_string(),
            context: "Never Too Much".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("local".to_string()),
            freshness: Some("fresh".to_string()),
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }
}
