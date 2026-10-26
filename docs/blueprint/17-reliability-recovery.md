# 17: Repair the listening journeys before adding features

Status: correction programme, 2026-10-26. Source baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. These chapters extend the original blueprint; the earlier decisions remain its history.

Use [23](23-recovery-roadmap.md) to finish the corrections. The completion check is a real journey: find music in a small terminal, play the intended track, queue an accessible playlist, move playback to another device, and keep it there beyond the recovery watchdog. A successful mutation receipt alone does not establish that outcome (D-042).

## The failures cross three boundaries

BK reported cramped search columns, an error after pressing `e` on a playlist result, and an error switching devices. The installed Homebrew binary was `0.1.109`; its operation log identifies the failed queue request and transfer. [The investigation](../research/2026-10-26-reliability-audit.md) separates those observations from source diagnosis.

| Journey | Verified defect or limit | Correction |
|---|---|---|
| Read search results | Search groups share horizontal space even in small terminals | One full-width active category, preserving query, selection and pagination |
| Change search category | Kind jump indexes raw results while selection uses filtered/sorted results | Use the visible list throughout |
| Play a playlist row | Enter in an opened playlist ignores the selected track | Send selected track with playlist context |
| Queue a discovered playlist | Spotify denied its items with HTTP 403; the adapter called it private without evidence | Preserve permission failure and name the playlist; avoid unsupported content promises |
| Inspect that playlist by CLI | Explicit playlist URI is looked up only among saved playlists | Direct references reach the provider; names still resolve through saved playlists |
| Move playback elsewhere | Connect handoff and broken connection share one recovery event | Separate deactivation from connection failure; cancel stale recovery |
| Understand health | CLI doctor bypasses daemon provider checks; added findings do not update health | Query the daemon and elect health after all findings |
| Build the native release | Ordinary CI lacks a native macOS smoke lane; no spotuify local runner is registered | Add native fake smoke and isolated opt-in local routing |

These are the findings supported by the examined code and runtime evidence. They do not establish that every other feature is defect-free. Remaining work has a spike or an issue rather than an assumed cause (D-053).

## The architecture remains daemon-backed

```text
CLI / TUI / MCP / macOS
          |
     typed local IPC
          |
Daemon: provider registry, playback clock, operation receipts, recovery intent
    |                   |                         |
SQLite/Tantivy    Spotify Web API       embedded librespot / audio
```

Keep the existing crate seams, protocol envelopes and storage schema. No migration is needed for this correction slice. `Deactivated` is an internal player event; clients receive the existing `PlaybackChanged` shape. The daemon owns music truth; each TUI owns only its query, focus, visible category and selection (D-043, D-044).

## The original mistakes were in the contracts

The search renderer assumed every media type needed an always-visible pane. That traded usable row width for simultaneous coverage. The playlist view treated a collection as the target even after the user selected one of its tracks. Provider errors lost their meaning when every unknown 403 became “private”. Recovery treated one upstream event as proof of a dead session without checking why that event is emitted. Diagnostics checked a smaller local state than the live process owns.

Preserve the strong parts already present: CLI mutation finalisation, typed provider errors, bounded IPC, a central playback clock and fake-provider smoke. Correct their broken handoffs rather than replace the architecture (D-045).

## Keep decisions explicit

The proposed experience is [Listening desk](18-listening-desk.md). BK asked for an intuitive TUI; he did not choose this name or approve every proposed interaction. Q1 records that distinction. API access and physical device behaviour remain empirical checks in [24](24-recovery-questions.md).
