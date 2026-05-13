use std::io::{self, Write};

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

use crate::analytics::StoredAnalyticsEvent;
use crate::protocol::{CacheStatus, CacheSyncSummary, ReindexStats};
use crate::spotify::{Device, MediaItem, Playback, Playlist, Queue};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Jsonl,
    Csv,
    Ids,
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

#[cfg(test)]
mod tests {
    use super::{write_basic_receipt, write_item_receipt, write_media_items, OutputFormat};
    use crate::spotify::{MediaItem, MediaKind};

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
        }];
        let mut out = Vec::new();

        write_media_items(&mut out, &items, OutputFormat::Csv).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "id,uri,type,name,subtitle,context,duration_ms\ntrack-1,spotify:track:track-1,track,\"Hello, \"\"Friend\"\"\",\"Artist, Featured\",Album,123000\n"
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
        };
        let mut out = Vec::new();

        write_item_receipt(&mut out, "play", &item, OutputFormat::Ids).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "spotify:track:track-1\n");
    }
}
