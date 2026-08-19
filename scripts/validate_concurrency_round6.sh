#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

for required_tool in cargo rustc python3; do
    if ! command -v "$required_tool" >/dev/null 2>&1; then
        printf 'required validation tool is unavailable: %s\n' "$required_tool" >&2
        exit 127
    fi
done

# Acceptance forbids stack-size and Tonepoet environment masks. Remove any
# inherited knobs before recording the toolchain or invoking a test binary.
unset RUST_MIN_STACK || true
while IFS='=' read -r name _; do
    case "$name" in
        TONEPOET_*) unset "$name" || true ;;
    esac
done < <(env)

LOG_DIR=${ROUND6_LOG_DIR:-target/round6-validation}
mkdir -p "$LOG_DIR"

printf 'rustc: %s\n' "$(rustc --version)" | tee "$LOG_DIR/toolchain.txt"
printf 'cargo: %s\n' "$(cargo --version)" | tee -a "$LOG_DIR/toolchain.txt"

python3 tools/verify_concurrency_corrective_round1.py | tee "$LOG_DIR/static-round1.log"
python3 tools/verify_concurrency_corrective_round4.py | tee "$LOG_DIR/static-round4.log"
python3 tools/verify_concurrency_corrective_round5.py | tee "$LOG_DIR/static-round5.log"
python3 tools/verify_concurrency_corrective_round6.py | tee "$LOG_DIR/static-round6.log"
python3 tools/verify_concurrency_corrective_round6_r1.py | tee "$LOG_DIR/static-round6-r1.log"
python3 tools/audit_concurrent_mutation_entrypoints.py | tee "$LOG_DIR/mutation-entrypoints.log"
python3 tools/audit_test_coordination_isolation.py | tee "$LOG_DIR/test-coordination.log"

cargo fmt --all -- --check

# Fail fast on the repaired contracts and their nearest round-5 neighbors. The
# names are also source-pinned by the static verifier, so a zero-match cargo
# filter cannot silently remove coverage.
for test_name in \
    artwork_plus_opens_crate_picker_with_images_and_file_manager_policy \
    artwork_plus_opens_crate_picker_with_images_and_explicit_non_mutating_policy \
    host_managed_create_defers_destination_existence_to_host \
    host_managed_named_duplicate_defers_type_and_destination_checks_to_host \
    host_managed_paste_defers_target_validation_and_suffix_planning_to_host \
    artwork_host_create_rejects_conflicting_tonepoet_claim_then_succeeds \
    artwork_host_rename_rejects_conflicting_tonepoet_claim_then_succeeds \
    artwork_host_duplicate_rejects_conflicting_tonepoet_claim_then_succeeds \
    artwork_host_delete_rejects_conflicting_tonepoet_claim_then_succeeds_with_picker_policy \
    artwork_host_paste_routes_exact_plan_to_hosted_file_task_worker \
    artwork_host_delete_preserves_non_recursive_picker_delete_policy \
    host_managed_picker_refresh_never_restores_interrupted_removal_without_host_admission \
    repackage_archive_cancelled_before_create_preserves_original_and_reports_cancel \
    preflight_reports_missing_rar_creator_before_extraction_work \
    repackage_archive_reports_missing_rar_creator_without_replacing_original \
    scoped_test_coordination_root_retirement_keeps_captured_family_path_alive \
    same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire \
    common_write_cleanup_keeps_process_reservation_until_shared_claim_retires \
    common_write_drop_cleanup_keeps_process_reservation_until_shared_claim_retires
do
    log="$LOG_DIR/focused-${test_name}.log"
    printf '\n=== focused regression: %s ===\n' "$test_name" | tee "$log"
    cargo test --workspace --no-fail-fast "$test_name" 2>&1 | tee -a "$log"
done

scan_workspace_log() {
    local log=$1
    if grep -Fq 'stack overflow' "$log"; then
        printf 'stack overflow observed in %s\n' "$log" >&2
        return 1
    fi
    if grep -E -q 'create persistent lease staging file .*No such file or directory' "$log"; then
        printf 'lease-staging ENOENT observed in %s\n' "$log" >&2
        return 1
    fi
}

# The acceptance criterion is repeated green under default libtest parallelism.
# Do not override test parallelism or add any environment ritual here.
for run in $(seq 1 5); do
    log="$LOG_DIR/workspace-run-${run}.log"
    printf '\n=== workspace run %s/5 ===\n' "$run" | tee "$log"
    cargo test --workspace --no-fail-fast 2>&1 | tee -a "$log"
    scan_workspace_log "$log"
done

for run in $(seq 1 50); do
    log="$LOG_DIR/sigpipe-consumer-nonzero-${run}.log"
    printf '\n=== SIGPIPE arbitration stress %s/50 ===\n' "$run" | tee "$log"
    cargo test --workspace --no-fail-fast \
        pipeline_reports_consumer_nonzero_and_closes_producer 2>&1 | tee -a "$log"
done

printf '\nround-6 dynamic acceptance gate passed\n'
