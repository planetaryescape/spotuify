use std::io::{self, Write};

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

use spotuify_core::{Device, MediaItem, Playback, Playlist, Queue, StoredAnalyticsEvent};
use spotuify_protocol::{CacheStatus, CacheSyncSummary, PlaylistCreateReceipt, ReindexStats};

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
                    item.map(|item| item.name.as_str()).unwrap_or(""),
                    item.map(|item| item.subtitle.as_str()).unwrap_or(""),
                    device.unwrap_or(""),
                    &playback.progress_ms.to_string(),
                    item.map(|item| item.uri.as_str()).unwrap_or(empty.as_str()),
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
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
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
            if let Some(item) = &queue.currently_playing {
                println!("NOW\t{}\t{}", item.name, item.uri);
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
                        candidate.explicit.map(bool_str).unwrap_or(""),
                        candidate.playable.map(bool_str).unwrap_or(""),
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
                        track.explicit.map(bool_str).unwrap_or(""),
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
        receipt.dry_run.map(bool_str).unwrap_or(""),
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
            println!("database_path,index_path,media_items,devices,playback_snapshots,playlists,playlist_items,recent_items,library_items,search_runs,search_results,sync_events,index_documents,last_sync_at_ms,last_search_at_ms");
            println!(
                "{}",
                csv_row(&[
                    &status.database_path,
                    &status.index_path,
                    &status.media_items.to_string(),
                    &status.devices.to_string(),
                    &status.playback_snapshots.to_string(),
                    &status.playlists.to_string(),
                    &status.playlist_items.to_string(),
                    &status.recent_items.to_string(),
                    &status.library_items.to_string(),
                    &status.search_runs.to_string(),
                    &status.search_results.to_string(),
                    &status.sync_events.to_string(),
                    &status.index_documents.to_string(),
                    &status
                        .last_sync_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    &status
                        .last_search_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ])
            );
            Ok(())
        }
        OutputFormat::Ids => {
            println!("{}", status.database_path);
            println!("{}", status.index_path);
            Ok(())
        }
        OutputFormat::Table => {
            println!("database\t{}", status.database_path);
            println!("index\t{}", status.index_path);
            println!("media_items\t{}", status.media_items);
            println!("playlists\t{}", status.playlists);
            println!("playlist_items\t{}", status.playlist_items);
            println!("recent_items\t{}", status.recent_items);
            println!("library_items\t{}", status.library_items);
            println!("search_runs\t{}", status.search_runs);
            println!("index_documents\t{}", status.index_documents);
            Ok(())
        }
    }
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
            println!("target,playback_snapshots,devices,playlists,playlist_items,recent_items,library_items,media_items");
            println!(
                "{}",
                csv_row(&[
                    summary.target.label(),
                    &summary.playback_snapshots.to_string(),
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
        D::CacheStatus { status } => return print_cache_status(status, format),
        D::Reindex { stats } => return print_reindex_stats(stats, format),
        D::Sync { summary } => return print_sync_summary(summary, format),
        D::Queue { queue } => return print_queue(queue, format),
        D::Playlists { playlists } => return print_playlists(playlists, format),
        D::Image { bytes } => {
            print!("<image {} bytes>", bytes.len());
        }
        D::Mutation { receipt } => {
            return print_basic_receipt(&receipt.action, &receipt.message, format);
        }
        D::PlaylistCreate { receipt } => {
            return print_playlist_create_receipt(receipt, format);
        }
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
                    op.subject_uris.first().map(String::as_str).unwrap_or("-"),
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
        D::OperationUndoResult {
            undo_op_id,
            succeeded,
            skipped,
            errors,
        } => match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "undo_op_id": undo_op_id,
                        "succeeded": succeeded,
                        "skipped": skipped,
                        "errors": errors,
                    }))?
                );
            }
            _ => {
                println!(
                    "undo {}: {} succeeded, {} skipped, {} error(s)",
                    undo_op_id,
                    succeeded,
                    skipped,
                    errors.len(),
                );
                for err in errors {
                    println!("  ! {err}");
                }
            }
        },
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

#[cfg(test)]
mod tests {
    use super::{
        write_basic_receipt, write_item_receipt, write_media_items, write_mutation_output,
        write_playlist_create_receipt, MutationOutput, OutputFormat,
    };
    use spotuify_core::{MediaItem, MediaKind};
    use spotuify_protocol::PlaylistCreateReceipt;

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
        }];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Csv).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "id,uri,type,name,subtitle,context,duration_ms\ntrack-1,spotify:track:track-1,track,\"Hello, \"\"Friend\"\"\",\"Artist, Featured\",Album,123000\n"
        );
    }

    #[test]
    fn json_media_output_has_stable_machine_fields() {
        let items = vec![media_item("track-1", "Never Too Much")];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Json).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
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

        write_media_items(&mut out, &items, OutputFormat::Jsonl).unwrap();

        let output = String::from_utf8(out).unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["uri"],
            "spotify:track:track-1"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["uri"],
            "spotify:track:track-2"
        );
    }

    #[test]
    fn ids_media_output_is_uri_per_line_without_headers() {
        let items = vec![
            media_item("track-1", "Never Too Much"),
            media_item("track-2", "Sweet Thing"),
        ];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Ids).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "spotify:track:track-1\nspotify:track:track-2\n"
        );
    }

    #[test]
    fn json_receipt_output_has_stable_shape() {
        let mut out = Vec::new();

        write_basic_receipt(&mut out, "pause", "Paused", OutputFormat::Json).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
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
        };
        let mut out = Vec::new();

        write_item_receipt(&mut out, "play", &item, OutputFormat::Ids).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "spotify:track:track-1\n");
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

        write_playlist_create_receipt(&mut out, &receipt, OutputFormat::Json).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
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

        write_mutation_output(&mut out, &receipt, OutputFormat::Json).unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["action"], "playlist-add");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["requested"], 2);
        assert_eq!(value["uris"][1], "spotify:track:2");
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
        }
    }
}
