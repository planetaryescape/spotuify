# spotuify runner activation evidence

Captured 2026-10-26 for the authorized `spotuify` runner setup and CI action correction.

## Repository snapshot

- Worker worktree: `/Users/bhekanik/code/bhekanik/.worktrees/spotuify-ci-20261026`
- Worker branch: `orchestrate/spotuify-ci-20261026`
- Worker base/head: `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`
- Clean base tree: `e726dddd0803a70a4619c68fa89df179d81554d0`
- Worker change: `.github/actions/self-hosted-rust-home/action.yml` only
- Action SHA-256: `5e551c8e622410911fbbd209f7c4b38b8a55a66653bf07cdc343d527d4388ba0`
- Source copied from integration worktree: `/Users/bhekanik/code/bhekanik/.worktrees/spotuify-recovery-20261026/.github/actions/self-hosted-rust-home/action.yml`

The action preserves separate `CARGO_HOME`, `RUSTUP_HOME`, `SCCACHE_DIR`, and port `4231`. It resolves `sccache` with `command -v`, rejects a missing/non-absolute/non-executable path with a clear error, then writes the absolute path to `RUSTC_WRAPPER` in `GITHUB_ENV`.

## Runner and service

- GitHub repository runner id `21`: `bk-m3max-spotuify`
- API status: `online`
- Labels: `self-hosted`, `macOS`, `ARM64`, `spotuify-builder`
- Runner version: `2.338.1`
- Downloaded asset SHA-256: `52c3fb801b931fbd454979bae5ec3443ea3da2c4a1b4c44fa546fb630d61a0a9`
- Install path: `/Users/bhekanik/.actions-runner/spotuify` (562 MB measured)
- Launch agent: `actions.runner.planetaryescape-spotuify.bk-m3max-spotuify`, loaded and running
- Service process: PID `78046`; listener process: PID `78066`
- Launch agent plist: `/Users/bhekanik/Library/LaunchAgents/actions.runner.planetaryescape-spotuify.bk-m3max-spotuify.plist`
- Runner credential files `.credentials` and `.credentials_rsaparams` have mode `600`; no credential values were captured.
- Service PATH resolves `agent-work` to `/Users/bhekanik/.dotfiles/bin/agent-work` and `sccache` to `/opt/homebrew/bin/sccache`.
- `pkg-config --modversion portaudio-2.0` returned `19`.

## Repository variable

`SPOTUIFY_MACOS_ARM64_RUNNER` is set at repository scope to:

```json
["self-hosted","macOS","ARM64","spotuify-builder"]
```

The current remote `main` SHA `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e` does not reference this variable. The candidate CI workflow routes trusted `main` runs to it and keeps pull requests on hosted runners.

## Checks

- GitHub runner inventory returned id `21`, `online`, and the four expected labels.
- `launchctl print gui/501/actions.runner.planetaryescape-spotuify.bk-m3max-spotuify` reported `state = running` and the dedicated runner path.
- `actionlint .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/release-please.yml` passed on the integration worktree.
- `bash scripts/cargo_runner_test.sh`, `bash scripts/ci_cargo_test.sh`, `bash scripts/release_change_scope_test.sh`, and `bash scripts/sign_macos_cli_test.sh` passed on the integration worktree.
- The changed composite action's embedded bash script passed `bash -n`.
- No Cargo build or application deployment was run.

## Remaining gate

Run the candidate native macOS fake-provider workflow after its CI changes are integrated. The setup evidence records runner readiness; it does not establish a successful native build or smoke journey.
