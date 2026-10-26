# Search still needs a decision about empty types and player height

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, TUI `app.rs` and `ui.rs`; [audit evidence](../research/2026-10-26-reliability-audit.md). The correction replaces result columns and fixes action targets. It retains these existing contracts:

- Up/down can cross media types; Tab jumps to the next type. Decide whether movement should remain within the active category.
- Categories without results have no selectable tab even when that category failed. Define a loading/empty/error category state before adding placeholders.
- Enter on a search playlist plays it; Enter on an artist opens its view. A consistent Open action needs an explicit product decision and a preserved back path.
- The persistent player consumes substantial vertical space in a 40x24 capture. Review a compact layout against Listening desk before changing global chrome.

Next action: review the actual 40/60/80/140-column captures and the interactive blueprint companion with BK, then approve one compact/full-size comp and key contract. Build each selected change through the existing action registry and shared daemon workflows. Verify human keyboard use as well as cell snapshots.

The source files are 15,087 and 7,652 lines at baseline. A separately authorised structural task can split search, playlist and device jobs with verbatim moves before behaviour changes; this issue does not authorise that restructure.
