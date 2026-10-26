#!/usr/bin/env bash
set -euo pipefail

BIN="target/${MATRIX_TARGET}/release/spotuify"
if [[ -z "${MACOS_SIGN_CERTIFICATE_BASE64:-}" || -z "${MACOS_SIGN_IDENTITY:-}" ]]; then
  echo 'macOS signing secrets not set; shipping the CLI binary unsigned.'
  exit 0
fi

# create-keychain changes the search list even when it is not made default.
original_keychains=()
keychain_listing="$(security list-keychains -d user)"
while IFS= read -r keychain; do
  original_keychains+=("$keychain")
done < <(printf '%s\n' "$keychain_listing" | python3 -c 'import shlex, sys; print("\n".join(shlex.split(sys.stdin.read())))')

signing_dir="$(mktemp -d "${RUNNER_TEMP}/spotuify-signing.XXXXXX")"
KEYCHAIN="$signing_dir/signing.keychain-db"
cleanup_signing() {
  status=$?
  trap - EXIT
  security list-keychains -d user -s "${original_keychains[@]}" || status=1
  if [[ -f "$KEYCHAIN" ]]; then
    security delete-keychain "$KEYCHAIN" || status=1
  fi
  rm -rf "$signing_dir" || status=1
  exit "$status"
}
trap cleanup_signing EXIT

KEYCHAIN_PWD="$(openssl rand -hex 20)"
security create-keychain -p "$KEYCHAIN_PWD" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PWD" "$KEYCHAIN"
printf '%s' "$MACOS_SIGN_CERTIFICATE_BASE64" | base64 --decode > "$signing_dir/cert.p12"
security import "$signing_dir/cert.p12" -k "$KEYCHAIN" \
  -P "$MACOS_SIGN_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$KEYCHAIN_PWD" "$KEYCHAIN" >/dev/null
# Select the identity's keychain without changing the user's default.
codesign --force --options runtime --timestamp --keychain "$KEYCHAIN" \
  --sign "$MACOS_SIGN_IDENTITY" "$BIN"
codesign --verify --strict --verbose=2 "$BIN"

if [[ -n "${MACOS_NOTARY_KEY_BASE64:-}" && -n "${MACOS_NOTARY_KEY_ID:-}" && -n "${MACOS_NOTARY_ISSUER_ID:-}" ]]; then
  printf '%s' "$MACOS_NOTARY_KEY_BASE64" | base64 --decode > "$signing_dir/notary.p8"
  ditto -c -k --keepParent "$BIN" "$signing_dir/spotuify-notarize.zip"
  xcrun notarytool submit "$signing_dir/spotuify-notarize.zip" \
    --key "$signing_dir/notary.p8" \
    --key-id "$MACOS_NOTARY_KEY_ID" \
    --issuer "$MACOS_NOTARY_ISSUER_ID" \
    --wait --timeout 20m
  echo "notarized $BIN"
else
  echo 'signed but not notarized (MACOS_NOTARY_* secrets not set).'
fi
