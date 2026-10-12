#!/usr/bin/env bash
# Non-compiling, repeatable archive-integrity check, safe on machines without Rust.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v sha256sum >/dev/null 2>&1 || { echo 'sha256sum required' >&2; exit 2; }
sha256sum -c --quiet R34_SOURCE_SHA256SUMS.txt
bash -n R34_ACCEPTANCE.sh
bash -n R34_OFFLINE_VERIFY.sh
printf 'R34 source manifest and shell scripts verified; Rust acceptance still required.\n'
