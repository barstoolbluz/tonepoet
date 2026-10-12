#!/usr/bin/env bash
# Deterministic non-compiling integrity/shell check for the R34C source tree.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
bash ./R34_OFFLINE_VERIFY.sh
bash -n R34C_ACCEPTANCE.sh
bash -n R34C_OFFLINE_VERIFY.sh
printf 'R34C offline integrity checks passed; compiled/physical gates are still open.\n'
