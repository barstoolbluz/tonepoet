#!/usr/bin/env bash
# TonePoet R33: focused regressions, preserved R32 gates and mandatory
# source-locked Reference requalification against a real SACD ISO.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"
command -v cargo >/dev/null 2>&1 || {
  echo 'Cargo unavailable: R33 requires a configured Rust 1.93+ build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'Unqualified Reference override is forbidden' >&2
  exit 2
}
if [[ ${R33_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR33 metadata authority, SACD transfer and gain-log regressions...\n'
cargo test --locked --lib r33_
cargo test --locked --bin tonepoet r33_
printf '\nFull preserved R32 acceptance and fresh source-locked Reference qualification...\n'
# R32 chains through R31/R30 and scripts/requalify_reference_r20.sh.
# Supply a real SACD ISO path as the first argument; never skip qualification.
bash ./R32_ACCEPTANCE.sh "$@"
