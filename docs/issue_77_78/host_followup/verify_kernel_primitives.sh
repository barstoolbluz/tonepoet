#!/usr/bin/env bash
# Supplemental ABI smoke test; does not substitute for Rust compilation/tests.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v cc >/dev/null || { echo "C compiler unavailable; skip" >&2; exit 2; }
bin=$(mktemp "${TMPDIR:-/tmp}/tonepoet-abi-XXXXXX")
trap 'rm -f "$bin"' EXIT
cc -O2 -Wall -Wextra -Werror kernel_primitives_smoke.c -o "$bin"
"$bin"
