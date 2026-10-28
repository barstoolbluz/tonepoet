#!/usr/bin/env bash
# Verifiable source and native Linux ABI checks; not a Rust test substitute.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)
cd "$repo"
bash docs/issue_77_78/verify_static.sh
sha256sum --quiet -c docs/issue_77_78/host_followup/LOCKED_REFERENCE_BASELINE_SHA256SUMS.txt
bash -n docs/issue_77_78/verify_host.sh docs/issue_77_78/verify_static.sh \
  docs/issue_77_78/host_followup/verify_followup_static.sh \
  docs/issue_77_78/host_followup/verify_kernel_primitives.sh
if [[ "$(uname -s)" == Linux ]] && command -v cc >/dev/null 2>&1; then
    bash docs/issue_77_78/host_followup/verify_kernel_primitives.sh
else
    echo 'Native C Linux ABI smoke: SKIP (not Linux or no C compiler)'
fi
echo 'Issue 77/78 follow-up static qualification: PASS'
