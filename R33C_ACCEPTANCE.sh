#!/usr/bin/env bash
# R33C corrective: the two demonstrated R33 defects, then the unchanged
# comprehensive R33->R32->R31->R30->Reference acceptance chain.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"
command -v cargo >/dev/null 2>&1 || {
  echo 'R33C acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'R33C does not permit an unqualified Reference override' >&2
  exit 2
}
if [[ ${R33C_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR33C focused regression and old DISCNUMBER admission tests...\n'
cargo test --locked --lib r30_set_unsigned_headroom_supports_pcm_and_dsd_without_losing_target_authority
cargo test --locked --lib r33c_
cargo test --locked --lib discnumber_bang_uses_validated_album_view_partition
printf '\nPreserved R33 / R32 / R31 / R30 gates and fresh Reference qualification...\n'
# The chained R30 acceptance runs r30_, r33_ is run in R33, and the final
# requalification requires an actual SACD ISO supplied as the first argument.
bash ./R33_ACCEPTANCE.sh "$@"
