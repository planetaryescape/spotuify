#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TASK_TEMP="$(mktemp -d)"
trap 'rm -rf "$TASK_TEMP"' EXIT
mkdir "$TASK_TEMP/bin"
cat > "$TASK_TEMP/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$CI_CARGO_RESULT"
if [[ "$RUNNER_ENVIRONMENT" == self-hosted ]]; then
  [[ "$CARGO_BUILD_JOBS" == 4 && "$RUST_TEST_THREADS" == 4 && "$RAYON_NUM_THREADS" == 4 && "$NEXTEST_TEST_THREADS" == 4 ]]
fi
exit 23
EOF
cat > "$TASK_TEMP/bin/agent-work" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$CI_QUEUE_RESULT"
export CARGO_BUILD_JOBS=5 RUST_TEST_THREADS=5 RAYON_NUM_THREADS=5 NEXTEST_TEST_THREADS=5
shift 2
exec "$@"
EOF
chmod +x "$TASK_TEMP/bin/cargo" "$TASK_TEMP/bin/agent-work"
export CI_CARGO_RESULT="$TASK_TEMP/cargo" CI_QUEUE_RESULT="$TASK_TEMP/queue"
for environment in github-hosted self-hosted; do
  set +e
  RUNNER_ENVIRONMENT="$environment" PATH="$TASK_TEMP/bin:$PATH" \
    "$ROOT/scripts/ci-cargo" test -p spotuify-core 'filter with spaces'
  code=$?
  set -e
  [[ "$code" == 23 ]]
  printf '%s\n' test -p spotuify-core 'filter with spaces' > "$TASK_TEMP/expected"
  diff -u "$TASK_TEMP/expected" "$CI_CARGO_RESULT"
  if [[ "$environment" == self-hosted ]]; then
    printf '%s\n' run -- env CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4 NEXTEST_TEST_THREADS=4 RAYON_NUM_THREADS=4 cargo test -p spotuify-core 'filter with spaces' > "$TASK_TEMP/expected"
    diff -u "$TASK_TEMP/expected" "$CI_QUEUE_RESULT"
  else
    [[ ! -e "$CI_QUEUE_RESULT" ]]
  fi
done
echo 'CI Cargo preserves arguments and exit status; self-hosted builds use the bounded shared queue.'
