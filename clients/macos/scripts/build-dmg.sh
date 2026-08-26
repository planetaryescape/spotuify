#!/usr/bin/env bash
set -euo pipefail

# Builds the Release Spotuify.app and packages it into a distributable DMG.
#
# Signing is automatic when a "Developer ID Application" identity is in the
# keychain (or SPOTUIFY_SIGN_IDENTITY is set): the app gets a hardened-runtime
# Developer ID signature and the DMG is signed. Set SPOTUIFY_NOTARY_PROFILE to a
# stored `notarytool` profile to also notarize + staple (opens with no Gatekeeper
# warning). With no identity (e.g. CI), it falls back to an ad-hoc unsigned DMG
# (users right-click -> Open on first launch).
#
# Usage:
#   scripts/build-dmg.sh [VERSION]
#   SPOTUIFY_VERSION=0.1.46 SPOTUIFY_NOTARY_PROFILE=spotuify-notary scripts/build-dmg.sh
#
# Version resolution order: $1 arg, then $SPOTUIFY_VERSION, then the workspace
# version in the repo-root Cargo.toml.
#
# Output: clients/macos/dist/Spotuify-<version>.dmg

# --- locate ourselves ---------------------------------------------------------
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
macos_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$macos_dir/../.." && pwd)"

cd "$macos_dir"

# --- resolve version ----------------------------------------------------------
resolve_version() {
  if [[ -n "${1:-}" ]]; then
    printf '%s' "$1"
    return 0
  fi
  if [[ -n "${SPOTUIFY_VERSION:-}" ]]; then
    printf '%s' "$SPOTUIFY_VERSION"
    return 0
  fi
  # Pull `version = "x.y.z"` from [workspace.package] in the root Cargo.toml.
  local cargo_toml="$repo_root/Cargo.toml"
  if [[ -f "$cargo_toml" ]]; then
    local v
    v="$(grep -m1 -E '^[[:space:]]*version[[:space:]]*=' "$cargo_toml" | sed -E 's/.*"([^"]+)".*/\1/')"
    if [[ -n "$v" ]]; then
      printf '%s' "$v"
      return 0
    fi
  fi
  return 1
}

if ! VERSION="$(resolve_version "${1:-}")" || [[ -z "$VERSION" ]]; then
  echo "::error:: could not resolve version (pass as arg, set SPOTUIFY_VERSION, or add version to Cargo.toml)" >&2
  exit 64
fi

echo "==> Building Spotuify DMG for version ${VERSION}"

# --- preflight ----------------------------------------------------------------
require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "::error:: required tool '$1' not found on PATH" >&2
    exit 69
  }
}
require xcodegen
require xcodebuild
require hdiutil
require shasum

# --- paths --------------------------------------------------------------------
derived_data="$macos_dir/build/dd"
export_dir="$macos_dir/build/release-export"
dist_dir="$macos_dir/dist"
staging_dir="$macos_dir/build/dmg-staging"
dmg_path="$dist_dir/Spotuify-${VERSION}.dmg"
cli_entitlements="$macos_dir/Support/spotuify-cli.entitlements"

# --- generate xcode project ---------------------------------------------------
echo "==> Generating Xcode project (xcodegen)"
xcodegen generate

# --- resolve signing identity -------------------------------------------------
# If a Developer ID Application identity is available (or SPOTUIFY_SIGN_IDENTITY
# is set), build a signed + hardened-runtime app; otherwise fall back to an
# ad-hoc-signed unsigned bundle (e.g. on CI runners without the cert).
SIGN_ID="${SPOTUIFY_SIGN_IDENTITY:-}"
if [[ -z "$SIGN_ID" ]] && command -v security >/dev/null 2>&1; then
  SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep -m1 'Developer ID Application' | sed -E 's/.*"(.*)".*/\1/')"
fi
TEAM_ID="${SPOTUIFY_TEAM_ID:-}"
if [[ -z "$TEAM_ID" && -n "$SIGN_ID" ]]; then
  TEAM_ID="$(printf '%s' "$SIGN_ID" | sed -E 's/.*\(([A-Z0-9]+)\)$/\1/')"
fi

# --- build Release config -----------------------------------------------------
# A plain `build` into a known DerivedData path yields a runnable Spotuify.app
# and pins MARKETING_VERSION so CFBundleShortVersionString matches the DMG name.
echo "==> Building Release configuration (xcodebuild)"
if [[ -n "$SIGN_ID" ]]; then
  echo "==> Signing with: $SIGN_ID (team ${TEAM_ID:-?})"
  xcodebuild \
    -project Spotuify.xcodeproj \
    -scheme Spotuify \
    -configuration Release \
    -derivedDataPath "$derived_data" \
    -destination 'generic/platform=macOS' \
    -allowProvisioningUpdates \
    MARKETING_VERSION="$VERSION" \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGN_IDENTITY="$SIGN_ID" \
    DEVELOPMENT_TEAM="$TEAM_ID" \
    CODE_SIGNING_REQUIRED=YES \
    CODE_SIGNING_ALLOWED=YES \
    ENABLE_HARDENED_RUNTIME=YES \
    OTHER_CODE_SIGN_FLAGS="--timestamp" \
    CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO \
    build
else
  echo "==> No Developer ID identity found; building unsigned"
  xcodebuild \
    -project Spotuify.xcodeproj \
    -scheme Spotuify \
    -configuration Release \
    -derivedDataPath "$derived_data" \
    -destination 'generic/platform=macOS' \
    MARKETING_VERSION="$VERSION" \
    CODE_SIGN_IDENTITY="-" \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGNING_ALLOWED=NO \
    build
fi

products_dir="$derived_data/Build/Products/Release"
app_path="$products_dir/Spotuify.app"

if [[ ! -d "$app_path" ]]; then
  echo "::error:: expected app not found at $app_path after build" >&2
  exit 70
fi
echo "==> Built app: $app_path"

# Stage a clean copy of the app so we never package stale DerivedData siblings
# (cp -R preserves the code signature).
rm -rf "$export_dir"
mkdir -p "$export_dir"
cp -R "$app_path" "$export_dir/Spotuify.app"
app_path="$export_dir/Spotuify.app"

# --- bundle the daemon+CLI binary (self-contained backend) --------------------
# Embed the universal `spotuify` binary so installing the app installs the whole
# backend (daemon + CLI). Source: $SPOTUIFY_BUNDLED_BIN if set, else build a
# universal (arm64+x86_64) binary from the workspace.
BUNDLED_BIN="${SPOTUIFY_BUNDLED_BIN:-}"
if [[ -z "$BUNDLED_BIN" ]]; then
  echo "==> Building universal spotuify binary (arm64 + x86_64)"
  ( cd "$repo_root" \
    && cargo build --release --bin spotuify --target aarch64-apple-darwin \
    && cargo build --release --bin spotuify --target x86_64-apple-darwin )
  BUNDLED_BIN="$macos_dir/build/spotuify-universal"
  lipo -create \
    "$repo_root/target/aarch64-apple-darwin/release/spotuify" \
    "$repo_root/target/x86_64-apple-darwin/release/spotuify" \
    -output "$BUNDLED_BIN"
fi
if [[ -f "$BUNDLED_BIN" ]]; then
  echo "==> Bundling spotuify binary into app (Contents/Resources/spotuify)"
  cp "$BUNDLED_BIN" "$app_path/Contents/Resources/spotuify"
  chmod +x "$app_path/Contents/Resources/spotuify"
else
  echo "::warning:: no bundled spotuify binary; app will rely on PATH/Homebrew"
fi

# --- (re)sign -----------------------------------------------------------------
# The binary was added after xcodebuild's signing, so sign it and re-seal the
# whole bundle (its CodeResources must cover the new binary for notarization).
if [[ -n "$SIGN_ID" ]]; then
  if [[ -f "$app_path/Contents/Resources/spotuify" ]]; then
    echo "==> Signing bundled binary"
    codesign --force --options runtime --timestamp \
      --entitlements "$cli_entitlements" \
      -s "$SIGN_ID" \
      "$app_path/Contents/Resources/spotuify"
  fi
  echo "==> Re-sealing app bundle with Developer ID"
  codesign --force --options runtime --timestamp -s "$SIGN_ID" "$app_path"
  codesign --verify --strict --verbose=2 "$app_path" 2>&1 | tail -3
elif command -v codesign >/dev/null 2>&1; then
  echo "==> Ad-hoc signing app bundle (unsigned distribution)"
  codesign --force --deep --sign - "$app_path" >/dev/null 2>&1 || \
    echo "    (ad-hoc sign skipped; bundle remains unsigned)"
fi

# --- package DMG --------------------------------------------------------------
mkdir -p "$dist_dir"
rm -f "$dmg_path"

# create-dmg builds a scratch `rw.*.dmg` next to its output and mounts it at
# /Volumes/dmg.<random>. An interrupted run leaves both behind and the next run
# then fails to create its own scratch image.
#
# Clean up only what THIS script's dist directory backs. Detaching every
# /Volumes/dmg.* would kill a concurrent create-dmg elsewhere on the machine
# (CI runners and a local release build can overlap), so the mount points come
# from `hdiutil info`, matched by the image path each one is backed by.
while IFS= read -r stale_mount; do
  [[ -n "$stale_mount" ]] || continue
  echo "==> Detaching stale mount $stale_mount"
  hdiutil detach "$stale_mount" -force >/dev/null 2>&1 || true
done < <(hdiutil info 2>/dev/null | awk -v dir="$dist_dir/" '
  /^={10,}/ { image = ""; next }
  /^image-path[[:space:]]*:/ {
    sub(/^image-path[[:space:]]*:[[:space:]]*/, "")
    image = $0
    next
  }
  image != "" && index(image, dir) == 1 {
    for (i = 1; i <= NF; i++) if (index($i, "/Volumes/") == 1) print $i
  }
')
rm -f "$dist_dir"/rw.*.dmg

if command -v create-dmg >/dev/null 2>&1; then
  echo "==> Packaging DMG with create-dmg"
  # create-dmg returns non-zero (2) when it succeeds but cannot codesign the
  # DMG; that is fine for an unsigned build, so tolerate it as long as the DMG
  # exists afterwards.
  #
  # --skip-jenkins drops the Finder AppleScript that positions the window and
  # icons. That step hung twice on 0.1.89 waiting for a Finder that never
  # answered; the layout it produces is cosmetic and the drag-to-Applications
  # link works without it.
  create-dmg \
    --volname "Spotuify ${VERSION}" \
    --app-drop-link 480 170 \
    --icon "Spotuify.app" 140 170 \
    --window-size 640 360 \
    --hide-extension "Spotuify.app" \
    --skip-jenkins \
    "$dmg_path" \
    "$app_path" || true
  if [[ ! -f "$dmg_path" ]]; then
    echo "::error:: create-dmg did not produce $dmg_path" >&2
    exit 70
  fi
else
  echo "==> create-dmg not found; packaging plain DMG with hdiutil"
  rm -rf "$staging_dir"
  mkdir -p "$staging_dir"
  cp -R "$app_path" "$staging_dir/Spotuify.app"
  ln -s /Applications "$staging_dir/Applications"
  hdiutil create \
    -volname "Spotuify ${VERSION}" \
    -srcfolder "$staging_dir" \
    -fs HFS+ \
    -format UDZO \
    -ov \
    "$dmg_path"
  rm -rf "$staging_dir"
fi

if [[ ! -f "$dmg_path" ]]; then
  echo "::error:: DMG not produced at $dmg_path" >&2
  exit 70
fi

# --- sign + notarize the DMG --------------------------------------------------
# Signing the DMG itself and notarizing makes it open with no Gatekeeper
# warning. Requires the Developer ID identity (SIGN_ID) and a stored notarytool
# profile (SPOTUIFY_NOTARY_PROFILE, e.g. created via `notarytool store-credentials`).
if [[ -n "$SIGN_ID" ]]; then
  echo "==> Signing DMG"
  codesign --force --timestamp --sign "$SIGN_ID" "$dmg_path"
  if [[ -n "${SPOTUIFY_NOTARY_PROFILE:-}" ]]; then
    echo "==> Notarizing DMG (profile: $SPOTUIFY_NOTARY_PROFILE) — this can take a few minutes"
    xcrun notarytool submit "$dmg_path" --keychain-profile "$SPOTUIFY_NOTARY_PROFILE" --wait
    echo "==> Stapling notarization ticket"
    xcrun stapler staple "$dmg_path"
    echo "==> Gatekeeper assessment:"
    spctl -a -vv -t install "$dmg_path" 2>&1 | head -3 || true
  else
    echo "    (SPOTUIFY_NOTARY_PROFILE unset; DMG is signed but NOT notarized)"
  fi
fi

# --- report -------------------------------------------------------------------
size="$(du -h "$dmg_path" | cut -f1 | tr -d '[:space:]')"
sha="$(shasum -a 256 "$dmg_path" | awk '{print $1}')"

echo ""
echo "==> DMG ready"
echo "    path:   $dmg_path"
echo "    size:   $size"
echo "    sha256: $sha"
