#!/bin/sh
set -eu

[ "$#" -eq 1 ]
folder=$(dirname "$1")
self_pgid=$(ps -o pgid= -p $$ | tr -d ' ')
parent_pgid=$(ps -o pgid= -p "$PPID" | tr -d ' ')
printf '%s %s\n' "$self_pgid" "$parent_pgid" > "$folder/pgids"
: > "$folder/started"
while [ ! -e "$folder/release" ]; do sleep 0.01; done
kill -TERM $$
