use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use spotuify_core::{MediaItem, MediaKind};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistPlan {
    pub title: String,
    pub description: String,
    pub target_length: u32,
    pub mood: String,
    pub theme_notes: Vec<String>,
    pub candidate_searches: Vec<String>,
    pub sequencing_notes: Vec<String>,
    pub exclusions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CandidateStatus {
    Resolved,
    Duplicate,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedTrackCandidate {
    pub position: usize,
    pub query: String,
    pub status: CandidateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<MediaItem>,
    pub confidence: f32,
    pub reason: String,
    #[serde(default)]
    pub alternatives: Vec<MediaItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playable: Option<bool>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistTrackSelection {
    pub position: usize,
    pub uri: String,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandidateIssue {
    pub query: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistMutationPreview {
    pub create_playlist: PlaylistCreateMetadata,
    pub add_uris: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistCreateMetadata {
    pub name: String,
    pub public: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistCreatePreview {
    pub action: String,
    pub dry_run: bool,
    pub name: String,
    pub requested_candidate_count: usize,
    pub added_item_count: usize,
    pub tracks: Vec<PlaylistTrackSelection>,
    pub unresolved: Vec<CandidateIssue>,
    pub duplicates_removed: Vec<CandidateIssue>,
    pub warnings: Vec<String>,
    pub mutation: PlaylistMutationPreview,
}

pub fn build_playlist_plan(brief: &str) -> Result<PlaylistPlan> {
    let brief = brief.trim();
    if brief.is_empty() {
        bail!("playlist brief is required");
    }

    let target_length = 12;
    let mut searches = Vec::new();
    push_unique(&mut searches, brief.to_string());
    push_unique(&mut searches, format!("{brief} music"));
    push_unique(&mut searches, format!("{brief} song"));

    let words = significant_words(brief);
    for word in &words {
        push_unique(&mut searches, word.clone());
    }
    if words.len() >= 2 {
        push_unique(&mut searches, words[..2].join(" "));
        push_unique(&mut searches, words[words.len() - 2..].join(" "));
    }
    searches.truncate(target_length as usize);

    Ok(PlaylistPlan {
        title: title_from_brief(brief),
        description: format!("A spotuify playlist plan for {brief}."),
        target_length,
        mood: "unspecified".to_string(),
        theme_notes: vec![brief.to_string()],
        candidate_searches: searches,
        sequencing_notes: vec![
            "Open with direct theme matches.".to_string(),
            "Group similar moods together and save the strongest closer for last.".to_string(),
        ],
        exclusions: Vec::new(),
    })
}

pub fn parse_plan(raw: &str) -> Result<PlaylistPlan> {
    serde_json::from_str(raw).context("failed to parse playlist plan JSON")
}

pub fn resolve_plan_candidates(
    plan: &PlaylistPlan,
    search_results: Vec<Vec<MediaItem>>,
) -> Vec<ResolvedTrackCandidate> {
    let mut seen = HashSet::new();
    plan.candidate_searches
        .iter()
        .enumerate()
        .map(|(index, query)| {
            let items = search_results.get(index).cloned().unwrap_or_default();
            resolve_query(index + 1, query, items, &mut seen)
        })
        .collect()
}

pub fn parse_candidates_jsonl(raw: &str) -> Result<Vec<ResolvedTrackCandidate>> {
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<ResolvedTrackCandidate>(line)
                .with_context(|| format!("failed to parse candidate JSONL line {}", index + 1))
        })
        .collect()
}

pub fn ensure_playlist_create_allowed(dry_run: bool, yes: bool) -> Result<()> {
    if dry_run || yes {
        return Ok(());
    }
    bail!("playlist creation requires --dry-run for preview or --yes to commit")
}

pub fn selected_track_uris(candidates: &[ResolvedTrackCandidate]) -> Vec<String> {
    playlist_tracks_from_candidates(candidates)
        .into_iter()
        .map(|track| track.uri)
        .collect()
}

pub fn playlist_tracks_from_candidates(
    candidates: &[ResolvedTrackCandidate],
) -> Vec<PlaylistTrackSelection> {
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    for candidate in candidates {
        if candidate.status != CandidateStatus::Resolved {
            continue;
        }
        let Some(uri) = candidate
            .chosen_uri
            .as_deref()
            .or_else(|| candidate.chosen.as_ref().map(|item| item.uri.as_str()))
        else {
            continue;
        };
        if !seen.insert(uri.to_string()) {
            continue;
        }
        let chosen = candidate.chosen.as_ref();
        tracks.push(PlaylistTrackSelection {
            position: tracks.len() + 1,
            uri: uri.to_string(),
            name: chosen.map_or_else(|| uri.to_string(), |item| item.name.clone()),
            subtitle: chosen.map(|item| item.subtitle.clone()).unwrap_or_default(),
            explicit: candidate
                .explicit
                .or_else(|| chosen.and_then(|item| item.explicit)),
        });
    }
    tracks
}

pub fn build_playlist_preview(
    name: &str,
    candidates: &[ResolvedTrackCandidate],
) -> PlaylistCreatePreview {
    let tracks = playlist_tracks_from_candidates(candidates);
    let unresolved = candidates
        .iter()
        .filter(|candidate| candidate.status == CandidateStatus::Unresolved)
        .map(|candidate| CandidateIssue {
            query: candidate.query.clone(),
            reason: candidate.reason.clone(),
            uri: None,
        })
        .collect::<Vec<_>>();
    let duplicates_removed = candidates
        .iter()
        .filter(|candidate| candidate.status == CandidateStatus::Duplicate)
        .map(|candidate| CandidateIssue {
            query: candidate.query.clone(),
            reason: candidate.reason.clone(),
            uri: candidate.chosen_uri.clone(),
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    if tracks.is_empty() {
        warnings.push("no resolved tracks to add".to_string());
    }
    if !unresolved.is_empty() {
        warnings.push(format!("{} unresolved candidate(s)", unresolved.len()));
    }
    if !duplicates_removed.is_empty() {
        warnings.push(format!(
            "{} duplicate candidate(s) removed",
            duplicates_removed.len()
        ));
    }
    let add_uris = tracks.iter().map(|track| track.uri.clone()).collect();
    PlaylistCreatePreview {
        action: "playlist-create".to_string(),
        dry_run: true,
        name: name.to_string(),
        requested_candidate_count: candidates.len(),
        added_item_count: tracks.len(),
        tracks,
        unresolved,
        duplicates_removed,
        warnings,
        mutation: PlaylistMutationPreview {
            create_playlist: PlaylistCreateMetadata {
                name: name.to_string(),
                public: false,
            },
            add_uris,
        },
    }
}

fn resolve_query(
    position: usize,
    query: &str,
    items: Vec<MediaItem>,
    seen: &mut HashSet<String>,
) -> ResolvedTrackCandidate {
    let tracks = items
        .into_iter()
        .filter(|item| item.kind == MediaKind::Track)
        .collect::<Vec<_>>();
    let chosen_index = tracks
        .iter()
        .position(|item| item.is_playable != Some(false) && !seen.contains(&item.uri));
    if let Some(chosen_index) = chosen_index {
        let chosen = tracks[chosen_index].clone();
        seen.insert(chosen.uri.clone());
        return ResolvedTrackCandidate {
            position,
            query: query.to_string(),
            status: CandidateStatus::Resolved,
            chosen_uri: Some(chosen.uri.clone()),
            confidence: confidence(chosen_index),
            reason: if chosen_index == 0 {
                "selected first playable track result".to_string()
            } else {
                "selected first playable non-duplicate track result".to_string()
            },
            alternatives: alternatives(&tracks, Some(chosen_index)),
            duplicate_of: None,
            explicit: chosen.explicit,
            playable: chosen.is_playable,
            source: chosen
                .source
                .clone()
                .map_or_else(|| "unknown".to_string(), |source| source.to_string()),
            chosen: Some(chosen),
        };
    }

    if let Some(duplicate_index) = tracks
        .iter()
        .position(|item| item.is_playable != Some(false) && seen.contains(&item.uri))
    {
        let duplicate = tracks[duplicate_index].clone();
        return ResolvedTrackCandidate {
            position,
            query: query.to_string(),
            status: CandidateStatus::Duplicate,
            chosen_uri: Some(duplicate.uri.clone()),
            chosen: Some(duplicate.clone()),
            confidence: confidence(duplicate_index),
            reason: "all playable matches were already selected".to_string(),
            alternatives: alternatives(&tracks, Some(duplicate_index)),
            duplicate_of: Some(duplicate.uri.clone()),
            explicit: duplicate.explicit,
            playable: duplicate.is_playable,
            source: duplicate
                .source
                .map_or_else(|| "unknown".to_string(), |source| source.to_string()),
        };
    }

    ResolvedTrackCandidate {
        position,
        query: query.to_string(),
        status: CandidateStatus::Unresolved,
        chosen_uri: None,
        chosen: None,
        confidence: 0.0,
        reason: if tracks.is_empty() {
            "no track results returned".to_string()
        } else {
            "no playable track results returned".to_string()
        },
        alternatives: tracks,
        duplicate_of: None,
        explicit: None,
        playable: None,
        source: "none".to_string(),
    }
}

fn alternatives(items: &[MediaItem], chosen_index: Option<usize>) -> Vec<MediaItem> {
    items
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != chosen_index)
        .map(|(_, item)| item.clone())
        .collect()
}

fn confidence(index: usize) -> f32 {
    match index {
        0 => 0.9,
        1 | 2 => 0.75,
        _ => 0.6,
    }
}

fn significant_words(brief: &str) -> Vec<String> {
    let stop = ["and", "the", "for", "with", "from", "into", "about"];
    brief
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| word.len() > 2 && !stop.contains(&word.as_str()))
        .collect()
}

fn title_from_brief(brief: &str) -> String {
    brief
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| {
            let lower = word.to_ascii_lowercase();
            if index > 0 && matches!(lower.as_str(), "and" | "or" | "the" | "of" | "to") {
                lower
            } else {
                capitalize_ascii(&lower)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize_ascii(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.trim().is_empty() && !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_schema_contains_required_agent_playlist_fields() {
        let plan =
            build_playlist_plan("exile and returning home").expect("playlist plan should build");
        let json = serde_json::to_value(&plan).expect("playlist plan should serialize");

        assert_eq!(json["title"], "Exile and Returning Home");
        assert_eq!(json["target_length"], 12);
        assert!(
            json["candidate_searches"]
                .as_array()
                .expect("candidate_searches should be an array")
                .len()
                >= 4
        );
        for field in [
            "description",
            "mood",
            "theme_notes",
            "sequencing_notes",
            "exclusions",
        ] {
            assert!(json.get(field).is_some(), "missing field {field}");
        }
    }

    #[test]
    fn resolution_deduplicates_tracks_prefers_playable_and_marks_unresolved() {
        let plan = PlaylistPlan {
            title: "Theme".to_string(),
            description: "desc".to_string(),
            target_length: 3,
            mood: "mood".to_string(),
            theme_notes: vec![],
            candidate_searches: vec!["one".into(), "two".into(), "three".into()],
            sequencing_notes: vec![],
            exclusions: vec![],
        };

        let candidates = resolve_plan_candidates(
            &plan,
            vec![
                vec![track("spotify:track:1", true)],
                vec![
                    track("spotify:track:1", true),
                    track("spotify:track:2", true),
                ],
                vec![track("spotify:track:3", false)],
            ],
        );

        assert_eq!(candidates[0].status, CandidateStatus::Resolved);
        assert_eq!(candidates[0].chosen_uri.as_deref(), Some("spotify:track:1"));
        assert_eq!(candidates[1].status, CandidateStatus::Resolved);
        assert_eq!(candidates[1].chosen_uri.as_deref(), Some("spotify:track:2"));
        assert_eq!(candidates[2].status, CandidateStatus::Unresolved);
        assert_eq!(
            selected_track_uris(&candidates),
            vec!["spotify:track:1", "spotify:track:2"]
        );
        assert!(!candidates[1].alternatives.is_empty());
    }

    #[test]
    fn playlist_create_preview_lists_tracks_unresolved_duplicates_and_mutation() {
        let candidates = vec![
            resolved("one", "spotify:track:1"),
            duplicate("two", "spotify:track:1"),
            unresolved("three"),
        ];

        let preview = build_playlist_preview("Exile", &candidates);

        assert!(preview.dry_run);
        assert_eq!(preview.added_item_count, 1);
        assert_eq!(preview.tracks[0].uri, "spotify:track:1");
        assert_eq!(preview.unresolved.len(), 1);
        assert_eq!(preview.duplicates_removed.len(), 1);
        assert_eq!(preview.mutation.create_playlist.name, "Exile");
        assert_eq!(preview.mutation.add_uris, vec!["spotify:track:1"]);
    }

    #[test]
    fn playlist_create_requires_preview_or_explicit_yes() {
        assert!(ensure_playlist_create_allowed(true, false).is_ok());
        assert!(ensure_playlist_create_allowed(false, true).is_ok());
        assert!(ensure_playlist_create_allowed(false, false).is_err());
    }

    fn track(uri: &str, playable: bool) -> MediaItem {
        MediaItem {
            id: spotuify_core::ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
            uri: uri.to_string(),
            name: uri.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 1,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("spotify".into()),
            freshness: None,
            explicit: Some(false),
            is_playable: Some(playable),
            ..Default::default()
        }
    }

    fn resolved(query: &str, uri: &str) -> ResolvedTrackCandidate {
        ResolvedTrackCandidate {
            position: 1,
            query: query.to_string(),
            status: CandidateStatus::Resolved,
            chosen_uri: Some(uri.to_string()),
            chosen: Some(track(uri, true)),
            confidence: 0.9,
            reason: "selected".to_string(),
            alternatives: Vec::new(),
            duplicate_of: None,
            explicit: Some(false),
            playable: Some(true),
            source: "spotify".to_string(),
        }
    }

    fn duplicate(query: &str, uri: &str) -> ResolvedTrackCandidate {
        ResolvedTrackCandidate {
            status: CandidateStatus::Duplicate,
            reason: "duplicate".to_string(),
            duplicate_of: Some(uri.to_string()),
            ..resolved(query, uri)
        }
    }

    fn unresolved(query: &str) -> ResolvedTrackCandidate {
        ResolvedTrackCandidate {
            position: 1,
            query: query.to_string(),
            status: CandidateStatus::Unresolved,
            chosen_uri: None,
            chosen: None,
            confidence: 0.0,
            reason: "no track results returned".to_string(),
            alternatives: Vec::new(),
            duplicate_of: None,
            explicit: None,
            playable: None,
            source: "none".to_string(),
        }
    }
}
