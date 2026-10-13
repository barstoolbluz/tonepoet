#!/usr/bin/env bash
# R36: validate the corrected core-row regression, preserve the full R34D ->
# Reference chain, and run all workspace targets on a qualified build host.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

command -v cargo >/dev/null 2>&1 || {
  echo 'R36 acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'R36 forbids an unqualified Reference override' >&2
  exit 2
}
if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo 'Usage: bash ./R36_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso' >&2
  echo 'A real SACD ISO is required by the preserved Reference qualification gate.' >&2
  exit 2
fi

printf '\nR36: check full delivered source integrity...\n'
sha256sum -c R36_SOURCE_SHA256SUMS.txt >/dev/null
if [[ ${R36_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR36: compile complete workspace and all targets...\n'
cargo check --locked --workspace --all-targets
printf '\nR36: corrected File tags core-row and metadata-sidecar regressions...\n'
cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority
cargo test --locked --lib editor_and_queue_share_one_track_per_file_metadata_sidecar_membership
cargo test --locked --lib multipart_metadata_sidecars_remain_independent_field_authorities
printf '\nR36: preserved R34D/R34C/R34/R33C and source-locked Reference qualification...\n'
# Do not call R35_ACCEPTANCE.sh: its historical R35 source inventory must
# reject R36's intentional test change. All R35 focused tests are run above.
bash ./R34D_ACCEPTANCE.sh "$1"
printf '\nR36: full workspace post-requalification test gate...\n'
cargo test --locked --workspace --all-targets
