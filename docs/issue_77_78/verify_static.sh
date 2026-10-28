#!/usr/bin/env bash
# Check the delivered source exactly as packaged. No compilation or side effects.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
sha256sum --quiet -c docs/issue_77_78/SOURCE_TREE_SHA256SUMS.txt
echo 'Issue 77/78 packaged source SHA-256: PASS'
