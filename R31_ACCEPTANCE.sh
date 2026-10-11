#!/usr/bin/env bash
# TonePoet R31 integration acceptance, run on a configured Rust/audio build host.
# A real SACD ISO is needed for the positive Reference smoke in R30_ACCEPTANCE.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"
command -v cargo >/dev/null 2>&1 || {
  echo 'Cargo unavailable: R31 compiled acceptance requires a configured build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'Unqualified Reference override is forbidden' >&2
  exit 2
}
if [[ ${R31_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR31 full-scale boundary and cache regressions...\n'
cargo test --locked --lib r31_
printf '\nPreserved R30 acceptance and fresh Reference requalification...\n'
bash ./R30_ACCEPTANCE.sh "$@"
