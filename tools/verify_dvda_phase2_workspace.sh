#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: tools/verify_dvda_phase2_workspace.sh [REPO_ROOT]

Runs the acceptance gate for the DVD-Audio Phase 2 bundle against the full
tonepoet workspace after the overlay patches have been applied.

Checks:
  1. python3 tools/audit_prepared_track_sample_rate.py .
  2. cargo fmt --all -- --check
  3. cargo check --workspace
  4. cargo test --workspace

Set DVDA_ALLOW_FORMAT_WRITE=1 to run `cargo fmt --all` before the check.
Set DVDA_REQUIRE_UDF_ISO_FIXTURES=1 in CI when paired DVD-A ISO fixtures are
available and real-UDF coverage must be mandatory.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

repo_root="${1:-.}"
cd "$repo_root"

if [[ ! -f Cargo.toml ]]; then
  echo "error: expected tonepoet repository root with Cargo.toml: $PWD" >&2
  exit 2
fi

if [[ ! -x tools/audit_prepared_track_sample_rate.py ]]; then
  echo "error: missing executable tools/audit_prepared_track_sample_rate.py" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 is required for the sample-rate migration audit" >&2
  exit 2
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required; cannot certify this bundle without cargo fmt/check/test" >&2
  exit 2
fi

python3 tools/audit_prepared_track_sample_rate.py .

if [[ "${DVDA_ALLOW_FORMAT_WRITE:-0}" == "1" ]]; then
  cargo fmt --all
else
  cargo fmt --all -- --check
fi

cargo check --workspace
cargo test --workspace
