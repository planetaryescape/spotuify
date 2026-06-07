use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use spotuify_core::{
    active_lyric_line_index, LyricLine, MediaItem, MediaKind, Playback, Playlist, SyncedLyrics,
};
use spotuify_protocol::{
    DaemonEvent, IpcClient, OperationSource, PlaybackCommand, Request, Response, ResponseData,
    SearchScopeData, SearchSortData, SearchSourceData, SyncTargetData,
};

use crate::output::{self, OutputFormat};
use crate::selection;

pub async fn ipc_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => output::print_playback(&playback, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_devices(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::DevicesList).await? {
        ResponseData::Devices { devices } => output::print_devices(&devices, format),
        _ => unexpected_response(),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn ipc_search(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
    limit: u32,
    pages: u8,
    play: bool,
    index: usize,
    sort: Option<SearchSortData>,
    format: OutputFormat,
) -> Result<()> {
    // pages > 1 uses the same streaming path as the TUI (Request::SearchStream
    // → 18 parallel daemon-spawned tasks → DaemonEvent::SearchPage events →
    // SearchComplete). Aggregate events synchronously before printing so the
    // CLI experience stays one-shot.
    let items = if pages > 1 {
        stream_search_aggregate(query, scope, source).await?
    } else {
        match daemon_request(Request::Search {
            query: query.to_string(),
            scope,
            source,
            limit,
            kinds: None,
            sort,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        }
    };

    if play {
        let item = selection::media_item_at_index(items, query, index)?;
        daemon_request(Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: item.uri.clone(),
            },
        })
        .await?;
        return output::print_item_receipt("play", &item, format);
    }

    output::print_media_items(&items, format)
}

/// CLI equivalent of TUI scroll-load-more: fetch a single page of
/// results for one media kind at a given offset.
pub async fn ipc_search_page(
    query: &str,
    kind: MediaKind,
    offset: u32,
    format: OutputFormat,
) -> Result<()> {
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let ack = client
        .request(Request::SearchPage {
            query: query.to_string(),
            kind: kind.clone(),
            offset,
            version,
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error { message, .. } => {
            anyhow::bail!("search-page request failed: {message}");
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for SearchPage event");
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                kind: ev_kind,
                offset: ev_offset,
                version: ev_version,
                items,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                return output::print_media_items(&items, format);
            }
            Ok(Ok(DaemonEvent::SearchFailed {
                kind: Some(ev_kind),
                offset: Some(ev_offset),
                version: ev_version,
                message,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                anyhow::bail!("{message}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
}

/// Connect, subscribe to events, fire `Request::SearchStream`, drain
/// pages until `SearchComplete`. Used by `spotuify search --pages 3`
/// to give CLI users the same 180-result capability as the TUI.
async fn stream_search_aggregate(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
) -> Result<Vec<MediaItem>> {
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let ack = client
        .request(Request::SearchStream {
            query: query.to_string(),
            scope,
            source,
            version,
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error { message, .. } => {
            anyhow::bail!("search-stream request failed: {message}");
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }

    let mut items: Vec<MediaItem> = Vec::new();
    let mut seen_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if std::time::Instant::now() >= deadline {
            // Partial results are better than a hard error; just return
            // what we collected so far. Mirrors TUI behavior when an
            // event leg lags.
            break;
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                version: ev_version,
                items: page_items,
                ..
            })) if ev_version == version => {
                for item in page_items {
                    if seen_uris.insert(item.uri.clone()) {
                        items.push(item);
                    }
                }
            }
            Ok(Ok(DaemonEvent::SearchComplete {
                version: ev_version,
                ..
            })) if ev_version == version => break,
            Ok(Ok(DaemonEvent::SearchFailed {
                version: ev_version,
                message,
                ..
            })) if ev_version == version => anyhow::bail!("{message}"),
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
    Ok(items)
}

pub async fn ipc_queue(command: Option<crate::QueueCommand>, format: OutputFormat) -> Result<()> {
    match command {
        Some(crate::QueueCommand::Add {
            uris,
            ids,
            search,
            many,
            format,
        }) => ipc_queue_add(uris, ids, search, many, format).await,
        None => match daemon_request(Request::QueueGet).await? {
            ResponseData::Queue { queue } => output::print_queue(&queue, format),
            _ => unexpected_response(),
        },
    }
}

pub async fn ipc_playlists(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaylistsList).await? {
        ResponseData::Playlists { playlists } => output::print_playlists(&playlists, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_resolve_tracks(from: &Path, format: OutputFormat) -> Result<()> {
    let raw = read_input(from)?;
    let plan = crate::agent_playlists::parse_plan(&raw)?;
    let mut results = Vec::with_capacity(plan.candidate_searches.len());
    for query in &plan.candidate_searches {
        let items = match daemon_request(Request::Search {
            query: query.clone(),
            scope: SearchScopeData::Track,
            // Plan resolution = catalog discovery, not library lookup.
            source: SearchSourceData::Spotify,
            limit: 50,
            kinds: None,
            sort: None,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        };
        results.push(items);
    }
    let candidates = crate::agent_playlists::resolve_plan_candidates(&plan, results);
    output::print_resolved_track_candidates(&candidates, format)
}

pub async fn ipc_play_query(
    query: &str,
    scope: SearchScopeData,
    format: OutputFormat,
) -> Result<()> {
    // `spotuify play <query>` is a "find anywhere and play" command
    // — catalog discovery, not library lookup. Limit=10 keeps the
    // search slim since we only consume the top result.
    ipc_search(
        query,
        scope,
        SearchSourceData::Spotify,
        10,
        1,
        true,
        1,
        None,
        format,
    )
    .await
}

pub async fn ipc_reindex(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Reindex).await? {
        ResponseData::Reindex { stats } => output::print_reindex_stats(&stats, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_cache_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::CacheStatus).await? {
        ResponseData::CacheStatus { status } => output::print_cache_status(&status, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_lyrics(command: crate::LyricsCommand) -> Result<()> {
    match command {
        crate::LyricsCommand::Show { track, format } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: track,
                force_refresh: false,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Follow {
            lines,
            lead,
            format,
        } => ipc_lyrics_follow(lines, lead.as_deref(), format.into()).await,
        crate::LyricsCommand::Fetch { track_uri, format } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: true,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Export { track_uri, output } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: false,
            })
            .await?;
            output::export_lyrics_lrc(&data, output.as_deref())
        }
        crate::LyricsCommand::Offset {
            track_uri,
            offset,
            format,
        } => {
            let offset_ms = parse_lyrics_offset(&offset)?;
            let data = daemon_request(Request::LyricsOffsetSet {
                track_uri,
                offset_ms,
            })
            .await?;
            output::print_response_data(&data, format)
        }
    }
}

pub async fn ipc_lyrics_follow(
    lines: usize,
    lead: Option<&str>,
    format: OutputFormat,
) -> Result<()> {
    if lines == 0 {
        anyhow::bail!("lyrics follow: --lines must be at least 1");
    }
    if !matches!(format, OutputFormat::Table | OutputFormat::Jsonl) {
        anyhow::bail!("lyrics follow supports only --format table or --format jsonl");
    }
    let lead_ms = lead.map_or(Ok(0), parse_lyrics_offset)?;

    spotuify_daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    client_request(&mut client, Request::SubscribeEvents).await?;

    let initial = client_playback_get(&mut client).await?;
    if initial.item.is_none() {
        anyhow::bail!("nothing is playing; run `spotuify play \"...\"` first");
    }

    let mut follower = LyricsFollower::new(initial, lead_ms);
    follower.refresh_lyrics(&mut client).await?;

    let mut stdout = std::io::stdout();
    let clear_screen = format == OutputFormat::Table && stdout.is_terminal();
    let mut last_render: Option<FollowRenderKey> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = ticker.tick() => {
                follower.render_if_changed(&mut stdout, lines, format, clear_screen, &mut last_render)?;
            }
            event = client.next_event() => {
                match event? {
                    DaemonEvent::PlaybackChanged { playback: Some(playback), .. } => {
                        let track_changed = follower.update_playback(playback);
                        if track_changed {
                            follower.refresh_lyrics(&mut client).await?;
                            last_render = None;
                        }
                    }
                    DaemonEvent::ShutdownRequested => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

async fn client_request(client: &mut IpcClient, request: Request) -> Result<ResponseData> {
    match client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

async fn client_playback_get(client: &mut IpcClient) -> Result<Playback> {
    match client_request(client, Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => Ok(playback),
        _ => unexpected_response(),
    }
}

async fn client_lyrics_get(client: &mut IpcClient, track_uri: &str) -> Result<FollowLyrics> {
    match client_request(
        client,
        Request::LyricsGet {
            track_uri: Some(track_uri.to_string()),
            force_refresh: false,
        },
    )
    .await?
    {
        ResponseData::Lyrics { lyrics, offset_ms } => Ok(FollowLyrics { lyrics, offset_ms }),
        _ => unexpected_response(),
    }
}

#[derive(Debug)]
struct LyricsFollower {
    playback: Playback,
    anchored_at: Instant,
    lyrics: Option<SyncedLyrics>,
    lyrics_offset_ms: i64,
    lead_ms: i64,
    status: Option<String>,
}

impl LyricsFollower {
    fn new(playback: Playback, lead_ms: i64) -> Self {
        Self {
            playback,
            anchored_at: Instant::now(),
            lyrics: None,
            lyrics_offset_ms: 0,
            lead_ms,
            status: None,
        }
    }

    fn update_playback(&mut self, playback: Playback) -> bool {
        let old_uri = self.playback.item.as_ref().map(|item| item.uri.as_str());
        let new_uri = playback.item.as_ref().map(|item| item.uri.as_str());
        let changed = old_uri != new_uri;
        self.playback = playback;
        self.anchored_at = Instant::now();
        if changed {
            self.lyrics = None;
            self.lyrics_offset_ms = 0;
            self.status = None;
        }
        changed
    }

    async fn refresh_lyrics(&mut self, client: &mut IpcClient) -> Result<()> {
        let Some(item) = self.playback.item.as_ref() else {
            self.lyrics = None;
            self.status = Some("No active track. Waiting for playback.".to_string());
            return Ok(());
        };
        let data = client_lyrics_get(client, &item.uri).await?;
        self.lyrics_offset_ms = data.offset_ms;
        match data.lyrics {
            Some(lyrics) if lyrics.synced => {
                self.lyrics = Some(lyrics);
                self.status = None;
            }
            Some(_) => {
                self.lyrics = None;
                self.status =
                    Some("synced lyrics unavailable; use `spotuify lyrics show`".to_string());
            }
            None => {
                self.lyrics = None;
                self.status = Some("No lyrics available for this track".to_string());
            }
        }
        Ok(())
    }

    fn render_if_changed<W: Write>(
        &self,
        writer: &mut W,
        lines: usize,
        format: OutputFormat,
        clear_screen: bool,
        last_render: &mut Option<FollowRenderKey>,
    ) -> Result<()> {
        let view = self.view_at(Instant::now());
        let key = FollowRenderKey::from(&view);
        if last_render.as_ref() == Some(&key) {
            return Ok(());
        }
        match format {
            OutputFormat::Table => write_follow_table(writer, &view, lines, clear_screen)?,
            OutputFormat::Jsonl => write_follow_jsonl(writer, &view)?,
            _ => unreachable!("validated before follow loop"),
        }
        *last_render = Some(key);
        Ok(())
    }

    fn view_at(&self, now: Instant) -> FollowView<'_> {
        let progress_ms = playback_progress_at(&self.playback, self.anchored_at, now);
        let active_line = self.lyrics.as_ref().and_then(|lyrics| {
            active_lyric_line_index(
                &lyrics.lines,
                progress_ms,
                self.lyrics_offset_ms.saturating_add(self.lead_ms),
            )
        });
        FollowView {
            playback: &self.playback,
            lyrics: self.lyrics.as_ref(),
            progress_ms,
            active_line,
            status: self.status.as_deref(),
        }
    }
}

#[derive(Debug)]
struct FollowLyrics {
    lyrics: Option<SyncedLyrics>,
    offset_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FollowRenderKey {
    track_uri: Option<String>,
    active_line: Option<usize>,
    is_playing: bool,
    status: Option<String>,
}

impl From<&FollowView<'_>> for FollowRenderKey {
    fn from(view: &FollowView<'_>) -> Self {
        Self {
            track_uri: view.playback.item.as_ref().map(|item| item.uri.clone()),
            active_line: view.active_line,
            is_playing: view.playback.is_playing,
            status: view.status.map(str::to_string),
        }
    }
}

#[derive(Debug)]
struct FollowView<'a> {
    playback: &'a Playback,
    lyrics: Option<&'a SyncedLyrics>,
    progress_ms: u64,
    active_line: Option<usize>,
    status: Option<&'a str>,
}

fn playback_progress_at(playback: &Playback, anchored_at: Instant, now: Instant) -> u64 {
    let elapsed_ms = if playback.is_playing {
        now.saturating_duration_since(anchored_at).as_millis() as u64
    } else {
        0
    };
    let progress = playback.progress_ms.saturating_add(elapsed_ms);
    playback
        .item
        .as_ref()
        .filter(|item| item.duration_ms > 0)
        .map_or(progress, |item| progress.min(item.duration_ms))
}

fn lyric_window(lines: &[LyricLine], active: usize, desired: usize) -> std::ops::Range<usize> {
    if lines.is_empty() || desired == 0 {
        return 0..0;
    }
    let desired = desired.min(lines.len());
    let before = desired / 2;
    let mut start = active.saturating_sub(before);
    if start + desired > lines.len() {
        start = lines.len().saturating_sub(desired);
    }
    start..(start + desired)
}

fn write_follow_table<W: Write>(
    writer: &mut W,
    view: &FollowView<'_>,
    lines: usize,
    clear_screen: bool,
) -> Result<()> {
    if clear_screen {
        write!(writer, "\x1B[2J\x1B[H")?;
    }
    let Some(item) = view.playback.item.as_ref() else {
        writeln!(writer, "No active track. Waiting for playback.")?;
        writer.flush()?;
        return Ok(());
    };
    writeln!(writer, "{} - {}", item.name, item.subtitle)?;
    writeln!(
        writer,
        "{}  {}",
        if view.playback.is_playing {
            "playing"
        } else {
            "paused"
        },
        format_duration(view.progress_ms)
    )?;
    if let Some(status) = view.status {
        writeln!(writer, "\n{status}")?;
        writer.flush()?;
        return Ok(());
    }
    let Some(lyrics) = view.lyrics else {
        writeln!(writer, "\nNo lyrics loaded yet.")?;
        writer.flush()?;
        return Ok(());
    };
    let active = view
        .active_line
        .unwrap_or(0)
        .min(lyrics.lines.len().saturating_sub(1));
    writeln!(writer)?;
    for index in lyric_window(&lyrics.lines, active, lines) {
        let marker = if index == active { ">" } else { " " };
        writeln!(writer, "{marker} {}", lyrics.lines[index].text)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_follow_jsonl<W: Write>(writer: &mut W, view: &FollowView<'_>) -> Result<()> {
    let item = view.playback.item.as_ref();
    if let Some(status) = view.status {
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "event": "status",
                "track_uri": item.map(|item| item.uri.as_str()),
                "track_name": item.map(|item| item.name.as_str()),
                "artist": item.map(|item| item.subtitle.as_str()),
                "is_playing": view.playback.is_playing,
                "progress_ms": view.progress_ms,
                "message": status,
            })
        )?;
        writer.flush()?;
        return Ok(());
    }
    let Some((lyrics, active)) = view.lyrics.zip(view.active_line) else {
        return Ok(());
    };
    let Some(line) = lyrics.lines.get(active) else {
        return Ok(());
    };
    writeln!(
        writer,
        "{}",
        serde_json::json!({
            "event": "line",
            "track_uri": item.map(|item| item.uri.as_str()),
            "track_name": item.map(|item| item.name.as_str()),
            "artist": item.map(|item| item.subtitle.as_str()),
            "is_playing": view.playback.is_playing,
            "progress_ms": view.progress_ms,
            "line_index": active,
            "line_start_ms": line.start_ms,
            "text": line.text.as_str(),
            "is_rtl": line.is_rtl,
        })
    )?;
    writer.flush()?;
    Ok(())
}

fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

pub async fn ipc_refresh_media(format: OutputFormat) -> Result<()> {
    let playback = daemon_current_playback().await?.unwrap_or_default();
    let item = playback
        .item
        .context("no active track; start playback before refreshing media")?;

    let cover_art = match item.image_url.clone() {
        Some(url) => match daemon_request(Request::CoverArt { url }).await? {
            ResponseData::CoverArt {
                path,
                cache_hit,
                bytes,
                ..
            } => Some(output::MediaRefreshCover {
                path,
                cache_hit,
                bytes,
            }),
            _ => return unexpected_response(),
        },
        None => None,
    };

    let lyrics_data = daemon_request(Request::LyricsGet {
        track_uri: Some(item.uri.clone()),
        force_refresh: true,
    })
    .await?;
    let lyrics = match lyrics_data {
        ResponseData::Lyrics { lyrics, offset_ms } => output::MediaRefreshLyrics {
            found: lyrics.is_some(),
            lines: lyrics.as_ref().map_or(0, |lyrics| lyrics.lines.len()),
            offset_ms,
        },
        _ => return unexpected_response(),
    };

    output::print_media_refresh(
        &output::MediaRefreshOutput {
            track_uri: item.uri,
            track_name: item.name,
            cover_art,
            lyrics,
        },
        format,
    )
}

pub async fn ipc_sync(target: SyncTargetData, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Sync { target }).await? {
        ResponseData::Sync { summary } => output::print_sync_summary(&summary, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_viz(command: crate::VizCommand) -> Result<()> {
    match command {
        crate::VizCommand::Enable => print_ack(Request::SetVizEnabled { enabled: true }).await,
        crate::VizCommand::Disable => print_ack(Request::SetVizEnabled { enabled: false }).await,
        crate::VizCommand::Source { kind } => {
            print_ack(Request::SetVizSource { kind: kind.into() }).await
        }
        crate::VizCommand::Status { format } => {
            match daemon_request(Request::GetVizStatus).await? {
                data @ ResponseData::VizStatus { .. } => output::print_response_data(&data, format),
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_mpris(command: crate::MprisCommand) -> Result<()> {
    match command {
        crate::MprisCommand::Status { format } => {
            match daemon_request(Request::GetDoctorReport).await? {
                ResponseData::DoctorReport { report } => {
                    let diagnostics = report
                        .system
                        .context("daemon did not return media-control diagnostics")?;
                    output::print_system_diagnostics(&diagnostics, format)
                }
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_play_uri(uri: &str, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: uri.to_string(),
            },
        })
        .await?,
        format,
    )
}

async fn print_ack(request: Request) -> Result<()> {
    match daemon_request(request).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_playback_command(action: PlaybackCommand, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::PlaybackCommand { command: action }).await?,
        format,
    )
}

pub async fn daemon_current_playback() -> Result<Option<Playback>> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => Ok(Some(playback)),
        _ => unexpected_response(),
    }
}

pub async fn ipc_transfer(device: &str, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::DeviceTransfer {
            device: device.to_string(),
        })
        .await?,
        format,
    )
}

pub async fn ipc_playlist(command: crate::PlaylistCommand) -> Result<()> {
    match command {
        crate::PlaylistCommand::Plan { brief, format } => {
            let plan = crate::agent_playlists::build_playlist_plan(&brief)?;
            output::print_playlist_plan(&plan, format)
        }
        crate::PlaylistCommand::Create {
            name,
            from,
            dry_run,
            yes,
            format,
        } => ipc_playlist_create(&name, &from, dry_run, yes, format).await,
        crate::PlaylistCommand::Tracks { playlist, format } => {
            match daemon_request(Request::PlaylistTracks {
                playlist,
                wait: true,
            })
            .await?
            {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
        crate::PlaylistCommand::Play { playlist, format } => {
            let playlists = match daemon_request(Request::PlaylistsList).await? {
                ResponseData::Playlists { playlists } => playlists,
                _ => return unexpected_response(),
            };
            let playlist = selection::resolve_playlist(&playlists, &playlist)?;
            ipc_play_uri(&selection::playlist_uri(&playlist.id), format).await
        }
        crate::PlaylistCommand::Add {
            playlist,
            uris,
            ids,
            dry_run,
            yes,
            format,
        } => ipc_playlist_add(&playlist, uris, ids, dry_run, yes, format).await,
        crate::PlaylistCommand::AddCurrent { playlist, format } => {
            let item = match daemon_request(Request::PlaybackGet).await? {
                ResponseData::Playback { playback } => {
                    playback.item.context("nothing is playing")?
                }
                _ => return unexpected_response(),
            };
            print_mutation(
                daemon_request(Request::PlaylistAddItems {
                    playlist,
                    uris: vec![item.uri],
                })
                .await?,
                format,
            )
        }
        crate::PlaylistCommand::Unfollow {
            playlist,
            yes,
            format,
        } => ipc_playlist_unfollow(&playlist, yes, format).await,
        crate::PlaylistCommand::SetImage {
            playlist,
            file,
            format,
        } => ipc_playlist_set_image(&playlist, &file, format).await,
    }
}

async fn ipc_playlist_set_image(playlist: &str, file: &Path, format: OutputFormat) -> Result<()> {
    use base64::Engine as _;

    // Spotify accepts only JPEG and caps the base64-encoded body at
    // 256 KB. Reading raw bytes ~ 192 KB roughly produces a 256 KB
    // encoded payload (base64 inflates by 4/3). Reject early so we
    // don't hand the daemon a payload Spotify will refuse anyway.
    const MAX_RAW_BYTES: usize = 192 * 1024;
    const MAX_ENCODED_BYTES: usize = 256 * 1024;

    let raw = if file == Path::new("-") {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read JPEG bytes from stdin")?;
        buf
    } else {
        std::fs::read(file).with_context(|| format!("failed to read {}", file.display()))?
    };
    if raw.is_empty() {
        anyhow::bail!("playlist set-image: input file is empty");
    }
    // Spotify accepts only JPEG. Sniff the SOI marker (FF D8 FF) before
    // we ship a non-JPEG that the daemon would round-trip just to get a
    // 400 back.
    if raw.len() < 3 || raw[0] != 0xff || raw[1] != 0xd8 || raw[2] != 0xff {
        anyhow::bail!(
            "playlist set-image: {} does not start with a JPEG SOI marker (FF D8 FF); Spotify accepts only JPEG",
            file.display()
        );
    }
    if raw.len() > MAX_RAW_BYTES {
        anyhow::bail!(
            "playlist set-image: {} is {} bytes; encoded payload would exceed Spotify's 256 KB cap. Re-export at a smaller size.",
            file.display(),
            raw.len()
        );
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
    if encoded.len() > MAX_ENCODED_BYTES {
        anyhow::bail!(
            "playlist set-image: encoded image is {} bytes, exceeds Spotify's 256 KB cap",
            encoded.len()
        );
    }

    let resolved = daemon_playlist(playlist).await?;
    print_mutation(
        daemon_request(Request::PlaylistSetImage {
            playlist: resolved.id.clone(),
            image_base64: encoded,
        })
        .await?,
        format,
    )
}

async fn ipc_playlist_unfollow(playlist: &str, yes: bool, format: OutputFormat) -> Result<()> {
    let resolved = daemon_playlist(playlist).await?;
    if !yes {
        confirm_playlist_unfollow(&resolved)?;
    }
    print_mutation(
        daemon_request(Request::PlaylistUnfollow {
            playlist: resolved.id.clone(),
        })
        .await?,
        format,
    )
}

fn confirm_playlist_unfollow(playlist: &Playlist) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("Confirmation required for `playlist unfollow`. Re-run with --yes.");
    }
    println!(
        "Unfollow `{}` ({})? This removes it from your library and is not reversible.",
        playlist.name, playlist.id
    );
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

async fn ipc_playlist_create(
    name: &str,
    from: &Path,
    dry_run: bool,
    yes: bool,
    format: OutputFormat,
) -> Result<()> {
    crate::agent_playlists::ensure_playlist_create_allowed(dry_run, yes)?;
    let raw = read_input(from)?;
    let candidates = crate::agent_playlists::parse_candidates_jsonl(&raw)?;
    let preview = crate::agent_playlists::build_playlist_preview(name, &candidates);
    if dry_run {
        return output::print_playlist_preview(&preview, format);
    }
    let uris = crate::agent_playlists::selected_track_uris(&candidates);
    if uris.is_empty() {
        anyhow::bail!("no resolved track URIs to add");
    }
    match daemon_request(Request::PlaylistCreate {
        name: name.to_string(),
        description: None,
        uris,
    })
    .await?
    {
        ResponseData::PlaylistCreate { receipt } => {
            output::print_playlist_create_receipt(&receipt, format)
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_library(command: crate::LibraryCommand) -> Result<()> {
    let (request, format) = match command {
        crate::LibraryCommand::Tracks { limit, format } => (Request::LibraryList { limit }, format),
        crate::LibraryCommand::SavedTracks {
            limit,
            offset,
            format,
        } => (Request::SavedTracks { limit, offset }, format),
        crate::LibraryCommand::Shows { limit, format } => (Request::SavedShows { limit }, format),
    };
    match daemon_request(request).await? {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_show(command: crate::ShowCommand) -> Result<()> {
    let crate::ShowCommand::Episodes {
        show,
        limit,
        offset,
        format,
    } = command;
    match daemon_request(Request::ShowEpisodes {
        show,
        limit,
        offset,
    })
    .await?
    {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_album(command: crate::AlbumCommand) -> Result<()> {
    let crate::AlbumCommand::Tracks { album, format } = command;
    match daemon_request(Request::AlbumTracks { album }).await? {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

/// Listening history grouped into sessions (or flattened to a chronological
/// track list with `--flat`). Merges local plays with Spotify recently-played.
pub async fn ipc_history(limit: u32, flat: bool, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::ListenSessions { limit }).await? {
        ResponseData::ListenSessions { sessions } => {
            if flat {
                let tracks: Vec<_> = sessions.into_iter().flat_map(|s| s.tracks).collect();
                output::print_media_items(&tracks, format)
            } else {
                output::print_listen_sessions(&sessions, format)
            }
        }
        _ => unexpected_response(),
    }
}

/// The cross-show episode feed: a flat, date-ordered list of episodes from all
/// followed podcasts.
pub async fn ipc_episodes(
    limit: u32,
    sort: spotuify_protocol::EpisodeSort,
    refresh: bool,
    format: OutputFormat,
) -> Result<()> {
    match daemon_request(Request::EpisodeFeed {
        limit,
        sort,
        refresh,
    })
    .await?
    {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

/// Report whether a newer spotuify release exists and how to upgrade.
pub async fn ipc_update(force: bool, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::CheckUpdate { force }).await? {
        ResponseData::UpdateStatus {
            update_available,
            current_version,
            latest_version,
            release_url,
            upgrade,
            checked_at_ms,
        } => output::print_update_status(
            update_available,
            &current_version,
            latest_version.as_deref(),
            release_url.as_deref(),
            &upgrade,
            checked_at_ms,
            format,
        ),
        _ => unexpected_response(),
    }
}

pub async fn ipc_artist(command: crate::ArtistCommand) -> Result<()> {
    match command {
        crate::ArtistCommand::Albums {
            artist,
            library_only,
            groups,
            format,
        } => match daemon_request(Request::ArtistAlbums { artist }).await? {
            ResponseData::MediaItems { mut items } => {
                // The daemon returns the full tagged discography; the toggle
                // and group filters are applied client-side (no refetch).
                if library_only {
                    items.retain(|item| item.in_library == Some(true));
                }
                if !groups.is_empty() {
                    let allowed: Vec<&str> = groups.iter().map(|g| g.as_api_str()).collect();
                    items.retain(|item| {
                        item.album_group
                            .as_deref()
                            .is_some_and(|group| allowed.contains(&group))
                    });
                }
                output::print_discography(&items, format)
            }
            _ => unexpected_response(),
        },
        crate::ArtistCommand::Followed { format } => {
            match daemon_request(Request::FollowedArtists { limit: 500 }).await? {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
        crate::ArtistCommand::Follow { artist, format } => {
            let data = daemon_request(Request::ArtistFollow {
                artist: normalize_artist_uri(&artist),
            })
            .await?;
            print_mutation(data, format)
        }
        crate::ArtistCommand::Unfollow { artist, format } => {
            let data = daemon_request(Request::ArtistUnfollow {
                artist: normalize_artist_uri(&artist),
            })
            .await?;
            print_mutation(data, format)
        }
    }
}

/// Accept either a full `spotify:artist:…` URI or a bare artist ID; the follow
/// endpoint routing needs a typed URI.
fn normalize_artist_uri(artist: &str) -> String {
    if artist.starts_with("spotify:") {
        artist.to_string()
    } else {
        format!("spotify:artist:{artist}")
    }
}

pub async fn ipc_reminder(command: crate::ReminderCommand) -> Result<()> {
    match command {
        crate::ReminderCommand::Create {
            uri,
            at,
            repeat,
            message,
            format,
        } => {
            let anchor_at_ms = parse_when(&at)?;
            let recurrence = spotuify_core::Recurrence::parse(&repeat).ok_or_else(|| {
                anyhow::anyhow!("invalid --repeat '{repeat}' (none|daily|weekly|monthly)")
            })?;
            match daemon_request(Request::ReminderCreate {
                media_uri: uri,
                anchor_at_ms,
                recurrence,
                tz: "UTC".to_string(),
                message,
            })
            .await?
            {
                ResponseData::ReminderCreated { reminder } => {
                    output::print_reminders(std::slice::from_ref(&reminder), format)
                }
                _ => unexpected_response(),
            }
        }
        crate::ReminderCommand::List { all, format } => {
            match daemon_request(Request::RemindersList {
                include_inactive: all,
            })
            .await?
            {
                ResponseData::Reminders { reminders } => {
                    output::print_reminders(&reminders, format)
                }
                _ => unexpected_response(),
            }
        }
        crate::ReminderCommand::Cancel { id, format: _ } => {
            print_ack(Request::ReminderCancel { id }).await
        }
    }
}

pub async fn ipc_notifications(command: crate::NotificationCommand) -> Result<()> {
    use spotuify_protocol::NotificationAction as NA;
    match command {
        crate::NotificationCommand::List { all, format } => {
            match daemon_request(Request::NotificationsList {
                include_archived: all,
            })
            .await?
            {
                ResponseData::Notifications { notifications } => {
                    output::print_notifications(&notifications, format)
                }
                _ => unexpected_response(),
            }
        }
        crate::NotificationCommand::Play { id, format: _ } => {
            print_ack(Request::NotificationAct {
                id,
                action: NA::Play,
                snooze_until_ms: None,
            })
            .await
        }
        crate::NotificationCommand::Queue { id, format: _ } => {
            print_ack(Request::NotificationAct {
                id,
                action: NA::Queue,
                snooze_until_ms: None,
            })
            .await
        }
        crate::NotificationCommand::Dismiss { id, format: _ } => {
            print_ack(Request::NotificationAct {
                id,
                action: NA::Dismiss,
                snooze_until_ms: None,
            })
            .await
        }
        crate::NotificationCommand::Snooze {
            id,
            snooze_for,
            format: _,
        } => {
            let dur = snooze_for
                .as_deref()
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(3_600_000);
            print_ack(Request::NotificationAct {
                id,
                action: NA::Snooze,
                snooze_until_ms: Some(spotuify_core::now_ms() + dur),
            })
            .await
        }
    }
}

/// Parse a `--at` value: `+2h`/`+30m`/`+3d`/`+1w`/`+45s`, `now`, `tomorrow`, or
/// an ISO-8601 datetime. Offsets/keywords are relative to local now; the result
/// is an absolute Unix epoch (ms).
fn parse_when(input: &str) -> Result<i64> {
    let s = input.trim();
    let now = chrono::Local::now();
    if let Some(rest) = s.strip_prefix('+') {
        return Ok(now.timestamp_millis() + parse_duration_ms(rest)?);
    }
    match s.to_ascii_lowercase().as_str() {
        "now" => return Ok(now.timestamp_millis()),
        "tomorrow" => return Ok((now + chrono::Duration::days(1)).timestamp_millis()),
        _ => {}
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp_millis());
    }
    anyhow::bail!("could not parse --at '{input}'; use +2h / +3d / +1w / tomorrow / ISO-8601")
}

/// Parse a bare duration like `15m`, `1h`, `4h`, `1d`, `1w`, `45s` into ms.
fn parse_duration_ms(input: &str) -> Result<i64> {
    let s = input.trim().trim_start_matches('+');
    if s.len() < 2 {
        anyhow::bail!("bad duration '{input}'");
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num
        .parse()
        .with_context(|| format!("bad duration number in '{input}'"))?;
    let ms = match unit {
        "s" => n * 1_000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        "w" => n * 604_800_000,
        other => anyhow::bail!("unknown duration unit '{other}' (use s/m/h/d/w)"),
    };
    Ok(ms)
}

pub async fn ipc_save_target(action: &str, target: &str, format: OutputFormat) -> Result<()> {
    let current = target.eq_ignore_ascii_case("current");
    let data = daemon_request(Request::LibrarySave {
        uri: (!current).then(|| target.to_string()),
        current,
    })
    .await?;
    match data {
        ResponseData::Mutation { mut receipt } => {
            receipt.action = action.to_string();
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

fn print_mutation(data: ResponseData, format: OutputFormat) -> Result<()> {
    match data {
        ResponseData::Mutation { receipt } => {
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

async fn ipc_queue_add(
    uris: Vec<String>,
    ids: Option<PathBuf>,
    search: Option<String>,
    many: bool,
    format: OutputFormat,
) -> Result<()> {
    match search {
        Some(query) => {
            if !uris.is_empty() || ids.is_some() {
                anyhow::bail!("provide URI(s), --ids, or --search, not more than one");
            }
            let items = match daemon_request(Request::Search {
                query: query.clone(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Spotify,
                limit: 50,
                kinds: None,
                sort: None,
            })
            .await?
            {
                ResponseData::SearchResults { items } => items,
                _ => return unexpected_response(),
            };
            let item = selection::media_item_at_index(items, &query, 1)?;
            daemon_request(Request::QueueAdd {
                uri: item.uri.clone(),
            })
            .await?;
            output::print_item_receipt("queue", &item, format)
        }
        None => {
            let selection = selection::resolve_uri_selection(
                uris,
                ids.as_deref(),
                "provide a URI or --search QUERY",
            )?;
            if many {
                // One aggregate request + receipt + undo entry.
                return match daemon_request(Request::QueueAddMany {
                    uris: selection.uris.clone(),
                })
                .await?
                {
                    ResponseData::Mutation { receipt } => {
                        output::print_basic_receipt(&receipt.action, &receipt.message, format)
                    }
                    _ => unexpected_response(),
                };
            }
            let mut errors = Vec::new();
            let mut succeeded = 0;
            for uri in &selection.uris {
                match daemon_request(Request::QueueAdd { uri: uri.clone() }).await {
                    Ok(ResponseData::Mutation { .. }) => succeeded += 1,
                    Ok(_) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: "unexpected response from daemon".to_string(),
                    }),
                    Err(err) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: err.to_string(),
                    }),
                }
            }
            let failed = errors.len();
            let receipt = output::MutationOutput {
                ok: failed == 0,
                action: "queue".to_string(),
                dry_run: Some(false),
                playlist: None,
                playlist_name: None,
                requested: selection.uris.len(),
                succeeded,
                failed,
                uris: selection.uris,
                errors,
                message: format!("Queued {succeeded} item(s)"),
            };
            output::print_mutation_output(&receipt, format)?;
            if receipt.failed > 0 {
                anyhow::bail!(
                    "partial mutation failure: queued {}, failed {}",
                    receipt.succeeded,
                    receipt.failed
                );
            }
            Ok(())
        }
    }
}

async fn ipc_playlist_add(
    playlist: &str,
    uris: Vec<String>,
    ids: Option<PathBuf>,
    dry_run: bool,
    yes: bool,
    format: OutputFormat,
) -> Result<()> {
    let selection = selection::resolve_uri_selection(
        uris,
        ids.as_deref(),
        "provide playlist URI(s), --ids FILE, or pipe IDs on stdin",
    )?;
    selection::ensure_track_or_episode_uris(&selection.uris)?;
    let playlist = daemon_playlist(playlist).await?;

    if dry_run {
        return output::print_mutation_output(
            &playlist_add_receipt(&playlist, &selection.uris, true, 0, Vec::new()),
            format,
        );
    }

    if selection.requires_confirmation() && !yes {
        confirm_playlist_add(&playlist, &selection.uris)?;
    }

    match daemon_request(Request::PlaylistAddItems {
        playlist: playlist.id.clone(),
        uris: selection.uris.clone(),
    })
    .await?
    {
        ResponseData::Mutation { .. } => output::print_mutation_output(
            &playlist_add_receipt(
                &playlist,
                &selection.uris,
                false,
                selection.uris.len(),
                Vec::new(),
            ),
            format,
        ),
        _ => unexpected_response(),
    }
}

async fn daemon_playlist(value: &str) -> Result<Playlist> {
    let playlists = match daemon_request(Request::PlaylistsList).await? {
        ResponseData::Playlists { playlists } => playlists,
        _ => return unexpected_response(),
    };
    Ok(selection::resolve_playlist(&playlists, value)?)
}

fn playlist_add_receipt(
    playlist: &Playlist,
    uris: &[String],
    dry_run: bool,
    succeeded: usize,
    errors: Vec<output::MutationOutputError>,
) -> output::MutationOutput {
    let failed = errors.len();
    let message = if dry_run {
        format!("Would add {} item(s) to {}", uris.len(), playlist.name)
    } else {
        format!("Added {succeeded} item(s) to {}", playlist.name)
    };
    output::MutationOutput {
        ok: failed == 0,
        action: "playlist-add".to_string(),
        dry_run: Some(dry_run),
        playlist: Some(playlist.id.clone()),
        playlist_name: Some(playlist.name.clone()),
        requested: uris.len(),
        succeeded,
        failed,
        uris: uris.to_vec(),
        errors,
        message,
    }
}

fn confirm_playlist_add(playlist: &Playlist, uris: &[String]) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Confirmation required for `playlist add`. Re-run with --yes or inspect with --dry-run."
        );
    }
    println!("Would add {} item(s) to {}", uris.len(), playlist.name);
    for uri in uris.iter().take(8) {
        println!("- {uri}");
    }
    if uris.len() > 8 {
        println!("... and {} more", uris.len() - 8);
    }
    print!("\nContinue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

async fn daemon_request(request: Request) -> Result<ResponseData> {
    spotuify_daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let response = client.request(request.clone()).await?;
    match response {
        Response::Ok { data } => Ok(data),
        Response::Error {
            kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
            message,
            ..
        } => handle_auth_revoked_then_retry(request, &message).await,
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

/// Interactive auto-recovery on `IpcErrorKind::AuthRevoked`. Prompts
/// Format OAuth progress as the same human-readable lines the CLI
/// has always emitted. Used by both `spotuify login` and the
/// auth-revoked retry path so the user sees identical output.
fn cli_login_progress(event: spotuify_spotify::auth::LoginProgress) {
    use spotuify_spotify::auth::LoginProgress;
    match event {
        LoginProgress::OpeningBrowser {
            auth_url,
            redirect_uri,
        } => {
            eprintln!("Opening Spotify authorization in your browser...");
            eprintln!("Spotify Dashboard Redirect URI should be one of:");
            eprintln!("  {redirect_uri}");
            eprintln!("  http://127.0.0.1/callback  (loopback dynamic-port allowlist)");
            eprintln!("Do not use the Website field, localhost, or a trailing slash.\n");
            eprintln!("If it does not open, visit:\n{auth_url}\n");
        }
        LoginProgress::BrowserLaunchFailed {
            auth_url,
            redirect_uri,
            error,
        } => {
            eprintln!(
                "Could not launch a browser automatically ({error}).\nOpen this URL in any browser:\n  {auth_url}\n(Waiting for the OAuth callback on {redirect_uri})"
            );
        }
        LoginProgress::WaitingForCallback => {}
        LoginProgress::Saved => {
            eprintln!("Spotify auth saved in the local auth file.");
        }
    }
}

/// the user on stdin; on Y, runs the same OAuth flow as `spotuify
/// login`, asks the daemon to drop its stale token cache, then
/// retries the original request exactly once.
///
/// Non-TTY callers (scripts, pipes) skip the prompt and exit with
/// the actionable error message — they have no way to answer "Y".
async fn handle_auth_revoked_then_retry(
    request: Request,
    original_message: &str,
) -> Result<ResponseData> {
    use std::io::{BufRead, IsTerminal, Write};

    eprintln!("Spotify session expired ({original_message}).");

    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "Spotify session expired and stdin is not a TTY; run `spotuify login` to recover"
        );
    }

    eprint!("Re-authenticate now? [Y/n] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("failed to read stdin")?;
    let answer = answer.trim();
    let consent = answer.is_empty() || matches!(answer, "y" | "Y" | "yes" | "Yes" | "YES");
    if !consent {
        anyhow::bail!("Aborted. Run `spotuify login` when you're ready to re-authenticate.");
    }

    eprintln!("Re-authenticating…");
    let config =
        spotuify_spotify::config::Config::load().context("failed to load Spotify config")?;
    spotuify_spotify::auth::login(&config, cli_login_progress)
        .await
        .context("OAuth flow failed")?;

    // Tell the daemon to drop its cached broken token + clear the
    // auth-revoked latch so the retry doesn't immediately fail again
    // with the same error.
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let _ = client.request(Request::ReloadAuth).await?;

    eprintln!("Retrying original command…");
    let mut retry_client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    match retry_client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

/// Phase 13 (P13-I) — reload the daemon's view of the config file
/// without a restart. Player backend swaps still require a restart;
/// the daemon returns a clear Ack with the message.
pub async fn ipc_reload() -> Result<()> {
    match daemon_request(Request::Reload).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

/// Phase 13 (P13-I) — request the daemon re-register its active player
/// backend (useful after a VPN flap).
pub async fn ipc_reconnect() -> Result<()> {
    match daemon_request(Request::Reconnect).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

fn unexpected_response<T>() -> Result<T> {
    anyhow::bail!("unexpected response from daemon")
}

fn parse_lyrics_offset(value: &str) -> Result<i64> {
    let raw = value.trim().strip_suffix("ms").unwrap_or(value.trim());
    raw.parse::<i64>()
        .with_context(|| format!("expected offset like +50ms or -200ms, got `{value}`"))
}

fn read_input(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw)?;
        return Ok(raw);
    }
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::{LyricLine, LyricsProvider, MediaKind};

    fn media_item(uri: &str, name: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    fn line(start_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            start_ms,
            text: text.to_string(),
            is_rtl: false,
        }
    }

    fn lyrics() -> SyncedLyrics {
        SyncedLyrics {
            provider: LyricsProvider::Lrclib,
            track_uri: "spotify:track:one".to_string(),
            lines: vec![
                line(0, "first"),
                line(1_000, "second"),
                line(2_000, "third"),
                line(3_000, "fourth"),
            ],
            fetched_at_ms: 1,
            synced: true,
            language: None,
            source_url: None,
        }
    }

    #[test]
    fn lyric_window_keeps_active_line_centered_when_possible() {
        let lines = lyrics().lines;

        assert_eq!(lyric_window(&lines, 2, 3), 1..4);
        assert_eq!(lyric_window(&lines, 0, 3), 0..3);
        assert_eq!(lyric_window(&lines, 3, 3), 1..4);
    }

    #[test]
    fn playback_progress_advances_while_playing_and_clamps_to_duration() {
        let anchor = Instant::now();
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 2_000)),
            is_playing: true,
            progress_ms: 1_500,
            ..Playback::default()
        };

        assert_eq!(
            playback_progress_at(&playback, anchor, anchor + Duration::from_secs(1)),
            2_000
        );

        let paused = Playback {
            is_playing: false,
            progress_ms: 1_500,
            ..playback
        };
        assert_eq!(
            playback_progress_at(&paused, anchor, anchor + Duration::from_secs(1)),
            1_500
        );
    }

    #[test]
    fn follow_view_applies_display_lead_to_active_line() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: false,
            progress_ms: 1_500,
            ..Playback::default()
        };
        let mut follower = LyricsFollower::new(playback, 700);
        follower.lyrics = Some(lyrics());

        let view = follower.view_at(follower.anchored_at);

        assert_eq!(view.active_line, Some(2));
    }

    #[test]
    fn jsonl_follow_output_emits_active_line_payload() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: true,
            progress_ms: 1_250,
            ..Playback::default()
        };
        let lyrics = lyrics();
        let view = FollowView {
            playback: &playback,
            lyrics: Some(&lyrics),
            progress_ms: 1_250,
            active_line: Some(1),
            status: None,
        };
        let mut out = Vec::new();

        write_follow_jsonl(&mut out, &view).expect("jsonl should write");

        let json: serde_json::Value =
            serde_json::from_slice(&out).expect("output should be valid JSON");
        assert_eq!(json["event"], "line");
        assert_eq!(json["track_uri"], "spotify:track:one");
        assert_eq!(json["line_index"], 1);
        assert_eq!(json["text"], "second");
    }

    #[test]
    fn table_follow_output_marks_current_line() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: false,
            progress_ms: 2_000,
            ..Playback::default()
        };
        let lyrics = lyrics();
        let view = FollowView {
            playback: &playback,
            lyrics: Some(&lyrics),
            progress_ms: 2_000,
            active_line: Some(2),
            status: None,
        };
        let mut out = Vec::new();

        write_follow_table(&mut out, &view, 3, false).expect("table should write");

        let rendered = String::from_utf8(out).expect("utf8 output");
        assert!(rendered.contains("paused  00:02"));
        assert!(rendered.contains("> third"));
    }
}
