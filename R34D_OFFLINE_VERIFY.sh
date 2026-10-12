#!/usr/bin/env bash
# Integrity and shell-syntax verification; not Rust/physical SACD qualification.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
bash ./R34C_OFFLINE_VERIFY.sh
bash -n R34D_ACCEPTANCE.sh R34D_OFFLINE_VERIFY.sh
printf 'R34D offline integrity checks passed; compiled/physical gates remain open.\n'
