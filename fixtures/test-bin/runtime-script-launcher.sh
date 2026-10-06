#!/bin/sh
set -eu

script_path=${TONEPOET_TEST_SCRIPT_PATH:-"$0"}
body=${TONEPOET_TEST_SCRIPT_BODY:-"${script_path}.tonepoet-body"}
export TONEPOET_TEST_SCRIPT_PATH="$script_path"
exec /bin/sh "$body" "$@"
