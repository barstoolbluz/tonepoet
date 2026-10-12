#!/usr/bin/env bash
# R34: reproduce all five reported isolated failures, test four new focused
# regressions, and preserve the R33C->R33->R32->...->Reference acceptance chain.
# Full qualification requires a configured Rust/audio host and a real SACD ISO.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v cargo >/dev/null 2>&1 || {
  echo 'R34 acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || {
  echo 'R34 does not permit an unqualified Reference override' >&2
  exit 2
}
if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo 'Usage: bash ./R34_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso' >&2
  echo 'A real SACD disc image is required for final Reference requalification.' >&2
  exit 2
fi
if [[ ${R34_CHECK_FORMAT:-0} == 1 ]]; then
  cargo fmt --all -- --check
fi
# Check production bins, integration tests, and workspace targets before
# running the focused lib tests: R33 previously missed a non-exhaustive match.
printf '\nR34: production and test-target compile gate...\n'
cargo check --locked --workspace --all-targets
printf '\nR34: reported failures and added boundaries...\n'
cargo test --locked --lib save_sacd_sidecar_writes_multi_tab_areas_without_implicit_mirror
cargo test --locked --lib sidecar_hybrid_disc_routes_mch_tracks_when_mch_area_selected
cargo test --locked --lib multifile_sidecar_untagged_carriers_save_album_performer_and_comment
cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority
cargo test --locked --lib transfer_picker_filter_is_canonical_audio_plus_cue_without_widening_global_audio
cargo test --locked --lib r34_
printf '\nR34: preserved acceptance and source-locked Reference requalification...\n'
bash ./R33C_ACCEPTANCE.sh "$@"
