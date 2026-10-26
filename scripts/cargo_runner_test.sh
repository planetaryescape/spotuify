#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TASK_TEMP="$(mktemp -d)"
trap 'rm -rf "$TASK_TEMP"' EXIT
mkdir "$TASK_TEMP/bin"
cat > "$TASK_TEMP/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$CARGO_TARGET_DIR" "$@" > "$RUNNER_RESULT"
exit 23
EOF
cat > "$TASK_TEMP/bin/pgrep" <<'EOF'
#!/usr/bin/env bash
touch "$REAPER_CALLED"
exit 1
EOF
chmod +x "$TASK_TEMP/bin/cargo" "$TASK_TEMP/bin/pgrep"
export RUNNER_RESULT="$TASK_TEMP/result" REAPER_CALLED="$TASK_TEMP/reaper"
for runner in cargo-test cargo-nextest; do
    set +e
    PATH="$TASK_TEMP/bin:$PATH" "$ROOT/scripts/$runner" -p spotuify-core 'filter with spaces'
    code=$?
    set -e
    [[ "$code" == 23 ]]
    [[ ! -e "$REAPER_CALLED" ]]
    if [[ "$runner" == cargo-test ]]; then
        printf '%s\n' "$ROOT/target-cli" test -p spotuify-core 'filter with spaces' > "$TASK_TEMP/expected"
    else
        printf '%s\n' "$ROOT/target-cli" nextest run -p spotuify-core 'filter with spaces' > "$TASK_TEMP/expected"
    fi
    diff -u "$TASK_TEMP/expected" "$RUNNER_RESULT"
done
echo 'Both Cargo runners preserve arguments/exit codes and leave other workflows alone.'
