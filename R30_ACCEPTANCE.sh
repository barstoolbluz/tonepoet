#!/usr/bin/env bash
# TonePoet R30 candidate acceptance. Run on a qualified Rust 1.93+ audio build host.
# This does not bypass or manufacture Reference qualification evidence.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"
command -v cargo >/dev/null 2>&1 || { echo 'Cargo unavailable: run this on a configured Rust/audio build host' >&2; exit 2; }
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || { echo 'Unqualified Reference override is forbidden' >&2; exit 2; }
if [[ ${R30_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR30 true-peak formatter, controls and command regressions...\n'
cargo test --locked --lib r30_
cargo test --locked --lib r27_reference_music_gain_excludes_only_internal_protection_without_exposing_it
printf '\nPreserved R29 and R29C regressions...\n'
cargo test --locked --lib r29c_
cargo test --locked --lib r29_
cargo test --locked --test settings_sentinel
cargo test --locked --bin tonepoet pcm_true_peak_cli_settings_tests
cargo test --locked --lib stored_audio_checksum::tests
cargo test --locked -p tonepoet-pipeline --lib true_peak
printf '\nFresh real-tool Reference qualification and full workspace acceptance...\n'
# Must execute after every final source/formatting edit to stages.rs and
# tonepoet-pipeline/src/settings.rs. This runner installs four coherent
# source-locked sidecars transactionally, tests freshness, runs the full
# workspace suite, release builds and required black-box smoke tests.
# Supply a real SACD ISO as $1 to cover the positive Reference conversion.
bash scripts/requalify_reference_r20.sh "$@"
printf '\nR30 scripted gates passed. Also perform the documented negative runtime-closure mismatch smoke.\n'
