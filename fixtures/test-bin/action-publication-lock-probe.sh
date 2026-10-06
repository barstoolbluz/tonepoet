#!/bin/sh
set -eu

[ "$#" -eq 3 ]
publication_lock=$1
script_marker=$2
environment_marker=$3

exec 9>"$publication_lock"
flock -n 9
rm -f "$publication_lock"
printf ok > "$script_marker"
printf %s "$TONEPOET_ALBUM_DIR" > "$environment_marker"
