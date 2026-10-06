#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$repo_dir"

python3 tools/verify_r34_immediate_contract_unobservable.py
python3 tools/verify_r33_fork_inheritance_class.py
python3 tools/verify_r32_parallel_fork_inheritance.py
python3 tools/verify_r31_test_coordination.py
python3 tools/audit_test_coordination_isolation.py
python3 tools/verify_r4_residual_flake_and_staging_corrective.py
python3 tools/verify_concurrency_corrective_round7.py

cargo test -p tonepoet --lib \
  concurrency::tests::journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::truncated_durable_descriptor_routes_by_path_to_lifecycle_cleanup_only \
  -- --exact
cargo test -p tonepoet --lib \
  disc::bluray_mapper::tests::ffprobe_command_failure_reports_status_and_stderr \
  -- --exact

runs="${R34_FULL_RUNS:-12}"
case "$runs" in
  ''|*[!0-9]*) echo "R34_FULL_RUNS must be a positive integer" >&2; exit 2 ;;
  0) echo "R34_FULL_RUNS must be greater than zero" >&2; exit 2 ;;
esac

for ((run = 1; run <= runs; run++)); do
  echo "=== R34 default-parallel tonepoet --lib run $run/$runs ==="
  cargo test -p tonepoet --lib
done
