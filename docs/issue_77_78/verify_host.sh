#!/usr/bin/env bash
# Native qualification entry point. Does not start a TUI or touch a live DB.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
printf 'TonePoet #77/#78 native test entry point\n'
command -v cargo >/dev/null || { echo 'cargo not found' >&2; exit 2; }
command -v rustc >/dev/null || { echo 'rustc not found' >&2; exit 2; }
rustc --version
cargo --version

# RUSTFLAGS overrides .cargo/config.toml, including the loop-vectorizer fix.
if [[ -n "${RUSTFLAGS:-}" ]] && [[ "${RUSTFLAGS}" != *'--vectorize-loops=false'* ]]; then
    echo 'RUSTFLAGS overrides the required LLVM loop-vectorizer workaround' >&2
    exit 2
fi

# Only the modified modules are targeted here. Full build/test qualification is
# a separate host gate; run the existing project suites after these pass.
cargo test --locked -p tonepoet --lib tui::event_loop::idle_redraw_tests -- --nocapture
cargo test --locked -p tonepoet --lib linux_descendant_snapshot -- --nocapture
cargo test --locked -p tonepoet --lib linux_clone3_requires_a_valid_leaf_descriptor -- --nocapture
cargo test --locked -p tonepoet --lib convert::script_supervisor::tests -- --nocapture

printf 'Focused unit tests passed. Host end-to-end and timing gates remain separate.\n'
