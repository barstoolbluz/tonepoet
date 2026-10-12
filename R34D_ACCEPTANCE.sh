#!/usr/bin/env bash
# R34D focused repair test, followed by established R34C-to-Reference gates.
# Final acceptance needs a configured Rust/audio build host and a real SACD ISO.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v cargo >/dev/null 2>&1 || {
  echo 'R34D acceptance requires Cargo and a configured Rust/audio build host' >&2
  exit 2
}
printf '\nR34D: duplicate TRACKNUMBER Save/reopen/repair plus physical-ID guard...\n'
cargo test --locked --lib r34d_multitab_save_can_repair_duplicate_track_numbers_without_cross_area_writes
printf '\nR34D: previously accepted R34C/R34/R33C and Reference qualification chain...\n'
bash ./R34C_ACCEPTANCE.sh "$@"
