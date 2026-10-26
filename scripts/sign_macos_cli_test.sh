#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TASK_TEMP="$(mktemp -d)"
trap 'rm -rf "$TASK_TEMP"' EXIT
mkdir "$TASK_TEMP/bin" "$TASK_TEMP/runner"
cat > "$TASK_TEMP/bin/security" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >> "$SIGN_SECURITY_CALLS"
case "$1" in
  list-keychains)
    if [[ "${4:-}" == -s ]]; then
      shift 4
      printf '%s\n' "$@" > "$SIGN_RESTORED_LIST"
    else
      printf '    "/Users/ci/Library/Keychains/login.keychain-db"\n    "/Users/ci/another keychain.keychain-db"\n'
    fi
    ;;
  create-keychain) touch "${!#}" ;;
  import) [[ "${SIGN_FAIL:-}" != import ]] || exit 17 ;;
  delete-keychain) rm -f "$2" ;;
esac
EOF
cat > "$TASK_TEMP/bin/codesign" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >> "$SIGN_CODESIGN_ARGS"
[[ "${SIGN_FAIL:-}" != codesign ]] || exit 19
EOF
cat > "$TASK_TEMP/bin/ditto" <<'EOF'
#!/usr/bin/env bash
touch "${!#}"
EOF
cat > "$TASK_TEMP/bin/xcrun" <<'EOF'
#!/usr/bin/env bash
[[ "${SIGN_FAIL:-}" != notary ]] || exit 21
EOF
chmod +x "$TASK_TEMP/bin/"*
export SIGN_SECURITY_CALLS="$TASK_TEMP/security-calls"
export SIGN_RESTORED_LIST="$TASK_TEMP/restored-list"
export SIGN_CODESIGN_ARGS="$TASK_TEMP/codesign-args"
export RUNNER_TEMP="$TASK_TEMP/runner" MATRIX_TARGET=aarch64-apple-darwin
export MACOS_SIGN_CERTIFICATE_BASE64=ZHVtbXk= MACOS_SIGN_IDENTITY=dummy MACOS_SIGN_CERTIFICATE_PASSWORD=dummy
export MACOS_NOTARY_KEY_BASE64=ZHVtbXk= MACOS_NOTARY_KEY_ID=dummy MACOS_NOTARY_ISSUER_ID=dummy
printf '%s\n' /Users/ci/Library/Keychains/login.keychain-db '/Users/ci/another keychain.keychain-db' > "$TASK_TEMP/expected-list"

for failure in none import codesign notary; do
  rm -f "$SIGN_SECURITY_CALLS" "$SIGN_RESTORED_LIST" "$SIGN_CODESIGN_ARGS"
  set +e
  SIGN_FAIL="$failure" PATH="$TASK_TEMP/bin:$PATH" "$ROOT/scripts/sign_macos_cli.sh"
  code=$?
  set -e
  case "$failure" in
    none) [[ "$code" == 0 ]] ;;
    import) [[ "$code" == 17 ]] ;;
    codesign) [[ "$code" == 19 ]] ;;
    notary) [[ "$code" == 21 ]] ;;
  esac
  diff -u "$TASK_TEMP/expected-list" "$SIGN_RESTORED_LIST"
  if grep -q default-keychain "$SIGN_SECURITY_CALLS"; then
    echo 'signing changed the default keychain' >&2
    exit 1
  fi
  grep -q delete-keychain "$SIGN_SECURITY_CALLS"
  [[ -z "$(ls -A "$RUNNER_TEMP")" ]]
  if [[ "$failure" != import ]]; then
    grep -qx -- --keychain "$SIGN_CODESIGN_ARGS"
  fi
done
echo 'macOS signing restores the keychain list and removes temporary secrets after success and import/sign/notary failures.'
