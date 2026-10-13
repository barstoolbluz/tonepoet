#!/usr/bin/env bash
# R35: confirm the selected-authority regression, preserve established
# one-track-per-file metadata-sidecar semantics, and run the full R34D ->
# Reference qualification chain. Final acceptance requires real SACD media.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

command -v cargo >/dev/null 2>&1 || {
  echo 'R35 acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'R35 forbids an unqualified Reference override' >&2
  exit 2
}
if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo 'Usage: bash ./R35_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso' >&2
  echo 'A physical SACD ISO is required for the existing Reference qualification gate.' >&2
  exit 2
fi

printf '\nR35: verify complete delivered source inventory...\n'
sha256sum -c R35_SOURCE_SHA256SUMS.txt >/dev/null
if [[ ${R35_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
printf '\nR35: compile entire workspace, including integration targets and binaries...\n'
cargo check --locked --workspace --all-targets
printf '\nR35: authority and original metadata-sidecar regression...\n'
cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority
cargo test --locked --lib editor_and_queue_share_one_track_per_file_metadata_sidecar_membership
cargo test --locked --lib multipart_metadata_sidecars_remain_independent_field_authorities
printf '\nR35: preserved R34D/R34C/R34/R33C and Reference gates...\n'
bash ./R34D_ACCEPTANCE.sh "$@"
printf '\nR35: complete workspace test gate following Reference requalification...\n'
cargo test --locked --workspace --all-targets
