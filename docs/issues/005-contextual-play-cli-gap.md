# Contextual track playback now has CLI and MCP parity

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `src/main.rs:253-263` and `crates/spotuify-cli/src/commands.rs:1836-1843,1875-1878`. `play-uri --help` on installed0.1.109 has URI, provider and output flags but no collection context. IPC already carries `PlaybackCommand::PlayUri.context_uri`, used by TUI collection playback.

The selected-row repair reuses that existing protocol capability. The CLI can play a track or a whole playlist, but cannot express “start this track within that playlist”. This is a pre-existing canonical-surface gap, not a reason to ask a human to reproduce it.

Status: implemented in repair source `ca361dde8cb4a7ab00eb6b52c1196991ec580673`; the canonical workspace run passes the real CLI context/Next integration regression. Release is pending; see the handover. The correction adds an optional context argument to `play-uri`, resolves/validates both resources and rejects cross-provider contexts before mutation. It supports the existing collection kinds and the Liked Songs sentinel. Keep existing no-context calls unchanged, generated help/reference current, and machine receipts stable.

Verify that a fake CLI journey starts a later track in a playlist and Next follows that playlist; invalid kind/provider conflicts fail clearly. Cover the same request on TUI and MCP. No new daemon ownership or special agent API is needed.
