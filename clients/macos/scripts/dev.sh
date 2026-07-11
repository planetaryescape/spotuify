#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
macos_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$macos_dir/../.." && pwd)"
derived_data="$macos_dir/build/dd-dev"
app_path="$derived_data/Build/Products/Debug/Spotuify.app"
spotuify_bin="$repo_root/target/release/spotuify"

usage() {
  printf 'Usage: %s [generate|build|test|live-test|daemon|run|check]\n' "$(basename "$0")"
}

require() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'error: required tool %s not found on PATH\n' "$1" >&2
    exit 69
  }
}

generate_project() {
  require xcodegen
  (cd "$macos_dir" && xcodegen generate)
}

build_app() {
  require xcodebuild
  generate_project
  xcodebuild \
    -project "$macos_dir/Spotuify.xcodeproj" \
    -scheme Spotuify \
    -configuration Debug \
    -derivedDataPath "$derived_data" \
    -destination 'platform=macOS' \
    build
}

test_app() {
  require xcodebuild
  generate_project
  xcodebuild \
    -project "$macos_dir/Spotuify.xcodeproj" \
    -scheme Spotuify \
    -configuration Debug \
    -derivedDataPath "$derived_data" \
    -destination 'platform=macOS' \
    test
}

test_live_daemon() {
  require xcodebuild
  start_dev_daemon
  generate_project
  xcodebuild \
    -project "$macos_dir/Spotuify.xcodeproj" \
    -scheme SpotuifyLiveDaemon \
    -configuration Debug \
    -derivedDataPath "$derived_data" \
    -destination 'platform=macOS' \
    test
}

start_dev_daemon() {
  (cd "$repo_root" && cargo build --release --bin spotuify)
  "$spotuify_bin" daemon start
}

run_app() {
  build_app
  start_dev_daemon
  open \
    -n \
    --env "SPOTUIFY_INSTANCE=spotuify-dev" \
    --env "SPOTUIFY_BIN=$spotuify_bin" \
    "$app_path"
}

case "${1:-check}" in
  generate)
    generate_project
    ;;
  build)
    build_app
    ;;
  test)
    test_app
    ;;
  live-test)
    test_live_daemon
    ;;
  daemon)
    start_dev_daemon
    ;;
  run)
    run_app
    ;;
  check)
    build_app
    test_app
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac
