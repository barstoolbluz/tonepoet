#!/usr/bin/env bash
# R20 focused tests and full workspace gate. Run inside the pinned Nix dev shell.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
command -v cargo >/dev/null || { printf '%s\n' 'ERROR: cargo unavailable. Run in the pinned Nix dev shell.' >&2; exit 127; }

# Run focused regressions separately so a failing fixture identifies its owner.
cargo test -p tonepoet rf64_ -- --nocapture
cargo test -p tonepoet hdcd_owned_tpdf_reverts_on_manual_off_on_every_toggle_route -- --nocapture
cargo test -p tonepoet hdcd_off_preserves_user_dither_and_independent_deemphasis_tpdf -- --nocapture
cargo test -p tonepoet hdcd_int16_automatic_tpdf_does_not_turn_into_preset_user_dither_policy -- --nocapture

# Must preserve the reported 2026-10-15 clean 7,501-test baseline; compare
# actual test count under the qualified host, not an inferred number here.
cargo test --workspace --no-fail-fast
