#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

REPORT_DIR="$ROOT/target"
REPORT="$REPORT_DIR/chunk2_1_1_settings_sentinel_validation_report.txt"
mkdir -p "$REPORT_DIR"

{
  echo "Chunk 2.1.1 settings sentinel validation report"
  echo "repository: $ROOT"
  echo "started_at_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo
} > "$REPORT"

run_and_record() {
  echo "+ $*" | tee -a "$REPORT"
  "$@" 2>&1 | tee -a "$REPORT"
}

if ! command -v cargo >/dev/null 2>&1; then
  {
    echo
    echo "FAIL: cargo is not available on PATH."
    echo "Install the Rust toolchain and rerun scripts/validate_chunk2_1_1_settings_sentinel.sh."
  } | tee -a "$REPORT" >&2
  exit 127
fi

{
  echo "+ cargo --version"
  cargo --version
  echo "+ rustc --version"
  rustc --version
  echo
} | tee -a "$REPORT"

run_and_record cargo fmt --all -- --check
run_and_record cargo test --workspace
run_and_record cargo test -p tonepoet-pipeline --all-features
run_and_record cargo clippy --workspace --all-targets -- -D warnings

{
  echo
  echo "PASS: Chunk 2.1.1 settings sentinel validation completed successfully."
  echo "finished_at_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "report: $REPORT"
} | tee -a "$REPORT"
