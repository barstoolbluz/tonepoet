#!/usr/bin/env bash
# R34C: one multitab SACD mapping regression, then all existing R34 gates.
# Full acceptance still requires a Rust/audio build host and a real SACD ISO.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v cargo >/dev/null 2>&1 || {
  echo 'R34C acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
printf '\nR34C: complete production and test-target compile gate...\n'
cargo check --locked --workspace --all-targets
printf '\nR34C: unequal-count, out-of-order hybrid SACD read/edit/save/reopen...\n'
cargo test --locked --lib r34c_multitab_save_uses_iso_area_counts_for_out_of_order_xml
printf '\nR34C: run existing R34, R33C and Reference requalification gates...\n'
bash ./R34_ACCEPTANCE.sh "$@"
