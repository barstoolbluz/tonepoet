#!/usr/bin/env bash
# Run ONLY on a configured Rust 1.93+ / real-audio-tool build host.
# This is an acceptance gate, never a certification bypass.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$HERE/tonepoet"
[[ -f "$REPO/Cargo.toml" ]] || { echo 'Expected handoff/tonepoet/Cargo.toml' >&2; exit 2; }
cd "$REPO"
command -v cargo >/dev/null || { echo 'cargo missing; use qualified build host' >&2; exit 2; }
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || { echo 'unqualified Reference override is forbidden' >&2; exit 2; }
# Run format review ahead of source-lock generation. The workspace is large and
# may have unrelated style; DO NOT mass-format the whole codebase without review.
if [[ ${R29_CHECK_FORMAT:-0} == 1 ]]; then
  command -v rustfmt >/dev/null || { echo 'rustfmt missing' >&2; exit 2; }
  cargo fmt --all -- --check
fi
printf 'R29 binary CLI tests...\n'
cargo test --locked --bin tonepoet pcm_true_peak_cli_settings_tests
printf 'R29 library tests...\n'
cargo test --locked --lib r29_
cargo test --locked --lib stored_audio_checksum::tests
printf 'Shared settings/ceiling tests...\n'
cargo test --locked -p tonepoet-pipeline --lib true_peak
# The supplied repo runner performs fresh candidate/report/cert/evidence/source
# lock generation, hash coherence, atomic install with rollback, freshness,
# functional smokes, the full workspace suite, and release builds.
unset TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH
bash scripts/requalify_reference_r20.sh "$@"
printf 'R29 build host acceptance runner succeeded. Inspect log cases and complete the documented negative runtime smoke.\n'
