#!/bin/sh
set -eu

[ "$#" -eq 2 ]
prefix=$1
marker=$2
printf '%s:%s\n' "$prefix" "${TONEPOET_ACTION_PHASE:-unknown}" >> "$marker"
