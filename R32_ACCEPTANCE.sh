#!/usr/bin/env bash
# TonePoet R32: target help + existing true-peak controls. Requires a configured
# Rust/audio build host; the chained R30 gate needs a real SACD ISO for its
# positive Reference smoke. No qualification override is permitted.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"
command -v cargo >/dev/null 2>&1 || {
  echo 'Cargo unavailable: R32 requires a configured Rust 1.93+ build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'Unqualified Reference override is forbidden' >&2
  exit 2
}
if [[ ${R32_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR32 true-peak help, hit-target, and renderer regressions...\n'
cargo test --locked --lib r32_
printf '\nPreserved R31 gate and chained Reference requalification...\n'
bash ./R31_ACCEPTANCE.sh "$@"
