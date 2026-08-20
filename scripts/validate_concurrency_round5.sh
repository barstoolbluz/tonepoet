#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

command -v cargo >/dev/null
command -v rustc >/dev/null

# Acceptance explicitly forbids stack/env masks.  Remove any inherited Tonepoet
# test knobs so this gate measures default production behavior and default
# libtest parallelism.
unset RUST_MIN_STACK || true
while IFS='=' read -r name _; do
    case "$name" in
        TONEPOET_*) unset "$name" || true ;;
    esac
done < <(env)

LOG_DIR=${ROUND5_LOG_DIR:-target/round5-validation}
mkdir -p "$LOG_DIR"

printf 'rustc: %s\n' "$(rustc --version)" | tee "$LOG_DIR/toolchain.txt"
printf 'cargo: %s\n' "$(cargo --version)" | tee -a "$LOG_DIR/toolchain.txt"

python3 tools/verify_concurrency_corrective_round1.py | tee "$LOG_DIR/static-round1.log"
python3 tools/verify_concurrency_corrective_round4.py | tee "$LOG_DIR/static-round4.log"
python3 tools/verify_concurrency_corrective_round5.py | tee "$LOG_DIR/static-round5.log"
python3 tools/audit_concurrent_mutation_entrypoints.py | tee "$LOG_DIR/mutation-entrypoints.log"
python3 tools/audit_test_coordination_isolation.py | tee "$LOG_DIR/test-coordination.log"

# Formatting is part of the code-quality gate and catches a useful class of
# malformed edits before the expensive repeated test runs.
cargo fmt --all -- --check

# Run the synchronized FLAC cleanup regressions first so a teardown-order
# regression fails fast before the expensive repeated workspace gate.
for test_name in \
    common_write_cleanup_keeps_process_reservation_until_shared_claim_retires \
    common_write_drop_cleanup_keeps_process_reservation_until_shared_claim_retires
do
    log="$LOG_DIR/focused-${test_name}.log"
    printf '\n=== focused FLAC teardown regression: %s ===\n' "$test_name" | tee "$log"
    cargo test --workspace --no-fail-fast "$test_name" 2>&1 | tee -a "$log"
done

for run in $(seq 1 5); do
    log="$LOG_DIR/workspace-run-${run}.log"
    printf '\n=== workspace run %s/5 ===\n' "$run" | tee "$log"
    cargo test --workspace --no-fail-fast 2>&1 | tee -a "$log"
    if grep -Fq 'stack overflow' "$log"; then
        printf 'stack overflow observed in %s\n' "$log" >&2
        exit 1
    fi
done

for run in $(seq 1 50); do
    log="$LOG_DIR/sigpipe-consumer-nonzero-${run}.log"
    printf '\n=== SIGPIPE arbitration stress %s/50 ===\n' "$run" | tee "$log"
    cargo test --workspace --no-fail-fast \
        pipeline_reports_consumer_nonzero_and_closes_producer 2>&1 | tee -a "$log"
done

printf '\nround-5 dynamic acceptance gate passed\n'
