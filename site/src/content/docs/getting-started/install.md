---
title: "Install"
description: "Install spotuify, set config, login, and verify playback."
---

Install `spotuify`, log in, then run `doctor` before you trust playback.

## Requirements

- Spotify account. Premium is required for local playback through the embedded librespot device (`spotuify-hume`).
- A terminal. Kitty or iTerm2 gives better cover art, but the app has text fallbacks.

```bash
spotuify --help
```

## Homebrew

```bash
brew tap planetaryescape/spotuify
brew trust --formula planetaryescape/spotuify/spotuify
brew install planetaryescape/spotuify/spotuify
spotuify --help
```

To update an existing Homebrew install:

```bash
brew update
brew upgrade planetaryescape/spotuify/spotuify
```

`brew trust --formula` keeps installs working when Homebrew tap-trust checks are enabled for third-party taps.

Release archives include SHA256 checksums and GitHub artifact provenance attestations. macOS binaries are not notarized yet, so Gatekeeper may still ask you to approve the first launch.

## Install script

For macOS and Linux x86_64 release archives, the installer downloads both the archive and its published `.sha256` file before installing:

```bash
curl -fsSLO https://raw.githubusercontent.com/planetaryescape/spotuify/main/install.sh
bash install.sh
spotuify --help
```

## Cargo

```bash
cargo install --git https://github.com/planetaryescape/spotuify --locked
spotuify --help
```

## Windows x64

Install the Windows x64 release zip from GitHub Releases. The snippet below resolves the latest release automatically; to pin a specific version, set `$Version` to its tag (for example `"v0.1.73"`) instead:

```powershell
$Version = (Invoke-RestMethod "https://api.github.com/repos/planetaryescape/spotuify/releases/latest").tag_name
$Archive = "spotuify-$Version-windows-x86_64.zip"
$Base = "https://github.com/planetaryescape/spotuify/releases/download/$Version"

Invoke-WebRequest "$Base/$Archive" -OutFile $Archive
Invoke-WebRequest "$Base/$Archive.sha256" -OutFile "$Archive.sha256"

$Expected = (Get-Content "$Archive.sha256").Split()[0].ToLowerInvariant()
$Actual = (Get-FileHash $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($Actual -ne $Expected) { throw "checksum mismatch for $Archive" }
```

Unzip it into a user-owned directory and put that directory on your `PATH`:

```powershell
$InstallDir = "$env:LOCALAPPDATA\spotuify\bin"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Expand-Archive $Archive -DestinationPath $InstallDir -Force

$env:Path = "$InstallDir;$env:Path"
[Environment]::SetEnvironmentVariable(
  "Path",
  "$InstallDir;" + [Environment]::GetEnvironmentVariable("Path", "User"),
  "User"
)

spotuify.exe --help
```

Install the user-level daemon service only when you want `spotuify` to start at login. On Windows this registers a Task Scheduler logon trigger:

```powershell
spotuify daemon install-service
```

Windows x64 is shipped as a release artifact and covered by CI check/test/build plus fake-provider smoke. Real login, playback, and Task Scheduler install are still beta until verified on a real Windows machine. Headless daemon media keys are also limited on Windows because SMTC needs a foreground window handle; keep the TUI open when you need global media-key handling.

Source install on Windows:

```powershell
cargo install --git https://github.com/planetaryescape/spotuify --locked `
  --no-default-features `
  --features "embedded-playback system-integrations loopback-cpal rodio-backend" `
  spotuify
spotuify.exe --help
```

## macOS app (.dmg)

Prefer a native window over the terminal? The SwiftUI menubar and player app lives in `clients/macos/`. It is a client of the same local daemon and bundles the `spotuify` CLI/backend when packaged.

The tag-driven CI release does not build the DMG today; it builds the CLI archives and Homebrew formula. DMGs are built locally with `clients/macos/scripts/build-dmg.sh` and attached to a release when available.

Download the latest DMG if the release includes one:

**[Download Spotuify.dmg](https://github.com/planetaryescape/spotuify/releases/latest/download/Spotuify.dmg)**

Verify the release checksum, then open the DMG and drag `Spotuify.app` to `Applications`:

```bash
curl -fsSLO https://github.com/planetaryescape/spotuify/releases/latest/download/Spotuify.dmg
curl -fsSLO https://github.com/planetaryescape/spotuify/releases/latest/download/Spotuify.dmg.sha256
shasum -a 256 -c Spotuify.dmg.sha256
```

The build script signs with a Developer ID when a signing identity is available and notarizes only when `SPOTUIFY_NOTARY_PROFILE` is set. If the published DMG is unsigned or not notarized, macOS may require the usual first-launch approval.

To download a specific version directly, the asset URL is versioned:

```bash
VERSION="0.1.56"
curl -fsSLO "https://github.com/planetaryescape/spotuify/releases/download/v${VERSION}/Spotuify-${VERSION}.dmg"
```

## Staying up to date

The daemon checks GitHub for newer releases (on start, then every few hours) and
tells you how to upgrade for the way you installed:

- **CLI:** `spotuify update` prints the current and latest versions plus the exact
  upgrade command (`--force` re-checks now, `--format json` for scripts).
- **macOS app:** an "update available" banner with a Download button; toggle it off
  under Settings.
- **TUI:** an upgrade banner with the upgrade command.

The check only contacts the public GitHub releases API and sends no identifying
data. Disable it entirely with `SPOTUIFY_NO_UPDATE_CHECK=1` in the daemon's
environment.

## Configure Spotify

`spotuify` is BYO Spotify app GA: the supported GA setup is for users who can create their own Spotify Developer app. It is not broad consumer no-developer setup yet; that would require a reviewed/shared Spotify app or a product decision to make first-party/keymaster auth the default.

Create a Spotify Developer app at the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) with redirect URI `http://127.0.0.1:8888/callback`, then add its client id to your config during onboarding. A client secret is optional for PKCE. Premium is required for playback.

The first-party/keymaster flow still exists for experiments, but it is opt-in with `SPOTUIFY_USE_FIRST_PARTY=1`.

## Login

```bash
spotuify login
spotuify doctor
```

What you get: a browser opens, you approve, and the OAuth token is stored in the local auth file under the app config directory. The doctor report tells you whether auth, daemon, device visibility, and Spotify API access work.

## Set your Spotify app

Set the client id in config, or export it before logging in:

```bash
export SPOTUIFY_CLIENT_ID=your-app-client-id
spotuify login
```

Apps in Spotify's Development Mode can be limited by Spotify policy. Apply for Extended Quota Mode if playlist or library writes return `403`.

## Start the daemon

```bash
spotuify daemon start
spotuify daemon status --format json
```

Install the platform user service when you want the daemon to survive shell sessions.

```bash
spotuify daemon install-service
```

## First sound

```bash
spotuify devices
spotuify play "imagine dragons" --type track
```

If playback fails with no active device, activate or transfer to the device you want:

```bash
spotuify transfer spotuify-hume
spotuify play "imagine dragons"
```

## See Also

- [First Run](/getting-started/first-run/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
