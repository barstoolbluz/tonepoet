#!/usr/bin/env bash
# R29C focused regressions, then the existing R29 transactional build-host gate.
# Run from the extracted TonePoet_R29C/ directory on a configured qualified host.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$HERE/tonepoet"
[[ -f "$REPO/Cargo.toml" ]] || { echo 'Expected sibling tonepoet/ source tree' >&2; exit 2; }
command -v cargo >/dev/null || { echo 'Cargo absent: run on configured build host' >&2; exit 2; }
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'Unqualified Reference override is forbidden' >&2; exit 2;
}
cd "$REPO"
printf 'R29C exhaustive settings fixture (build and test)...\n'
cargo test --locked --test settings_sentinel
printf 'R29C completed-Reference writer presentation regressions...\n'
cargo test --locked --lib r29c_
printf 'Run existing R29 regressions and transactional Reference requalification...\n'
bash "$HERE/R29_ACCEPTANCE.sh" "$@"
printf 'R29C scripted gate completed; document the required positive real-SACD and negative runtime-mismatch smokes.\n'
