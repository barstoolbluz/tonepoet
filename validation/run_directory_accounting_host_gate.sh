#!/usr/bin/env bash
# Run after extracting the source on a Rust 1.93+ host with native libraries.
# Exit on the first failed gate, preserving its complete log.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
logs="${1:-"$repo/../directory-accounting-gate-logs"}"
mkdir -p "$logs"
cd "$repo"
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo 'Rust toolchain unavailable. Install the version specified by Cargo.toml first.' >&2
    exit 127
fi
{
    date -u '+Gate started (UTC): %Y-%m-%dT%H:%M:%SZ'
    rustc --version
    cargo --version
} | tee "$logs/toolchain.txt"
run() {
    name="$1"
    shift
    printf '\n=== %s ===\n' "$name"
    "$@" 2>&1 | tee "$logs/$name.log"
}
run sqlite_python python3 validation/directory_accounting_sqlite_gate.py
run migration_regressions cargo test --locked --lib directory_accounting_v30_migration_tests
run repository_regressions cargo test --locked --lib directory_accounting::repository::tests
run scanner_regressions cargo test --locked --lib directory_accounting::scanner::tests
run album_legacy_retirement cargo test --locked --lib cached_album_classification_does_not_relaunch_retired_stats_after_debounce
run collection_legacy_retirement cargo test --locked --lib cached_collection_classification_does_not_relaunch_retired_stats_after_debounce
run full_workspace cargo test --locked --workspace
run formatting cargo fmt --all -- --check
printf 'All automated host gates PASSED. See %s\n' "$logs"
