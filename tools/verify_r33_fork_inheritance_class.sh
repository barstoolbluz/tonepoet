#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$repo_dir"

python3 tools/verify_r33_fork_inheritance_class.py
python3 tools/verify_r32_parallel_fork_inheritance.py
python3 tools/verify_r31_test_coordination.py
python3 tools/audit_test_coordination_isolation.py
python3 tools/verify_r4_residual_flake_and_staging_corrective.py
python3 tools/verify_concurrency_corrective_round7.py

cargo test -p tonepoet --lib \
  concurrency::tests::exported_ephemeral_lifetime_keeps_descriptor_visible_until_final_holder_closes \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::into_lease_preserves_detached_ephemeral_authority \
  -- --exact
cargo test -p tonepoet --lib \
  db::tests::empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved \
  -- --exact
cargo test -p tonepoet --lib \
  db::tests::new_queue_scope_reclaims_abandoned_empty_scope_without_prior_load \
  -- --exact
cargo test -p tonepoet --lib \
  db::tests::recovered_interrupted_rows_release_reservations_immediately \
  -- --exact
cargo test -p tonepoet --lib \
  convert::pipeline::stages::tests::scratch_backed_live_companion_snapshot_is_protected_then_reaped_after_execution_release \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::journal_operation_deliberate_lifetime_export_remains_live_until_export_closes \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::same_process_journal_coholder_shares_deliberate_export_authority \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately \
  -- --exact
cargo test -p tonepoet --lib \
  concurrency::tests::lifecycle_retirement_waits_out_accidental_cloexec_coholder_but_not_live_authority \
  -- --exact
cargo test -p tonepoet --lib \
  convert::script_supervisor::tests::control_channel_eof_is_treated_as_cancellation \
  -- --exact

runs="${R33_FULL_RUNS:-10}"
case "$runs" in
  ''|*[!0-9]*) echo "R33_FULL_RUNS must be a positive integer" >&2; exit 2 ;;
  0) echo "R33_FULL_RUNS must be greater than zero" >&2; exit 2 ;;
esac

for ((run = 1; run <= runs; run++)); do
  echo "=== R33 default-parallel tonepoet --lib run $run/$runs ==="
  cargo test -p tonepoet --lib
done
