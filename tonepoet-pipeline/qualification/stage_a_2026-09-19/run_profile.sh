#!/usr/bin/env bash
set -u
W=$HOME/.cache/tonepoet-stage-a
T=/home/daedalus/dev/tonepoet/target/release/tonepoet
export RUST_LOG=warn
FLAC3=("$W/src/Kiss - Rock and Roll Over (1976) [FLAC] {Remaster}" "$W/src/Journey - Evolution (1979) [FLAC]" "$W/src/Fragile (2001 Japanese Non-HDCD Remaster)")
CUE="$W/src/Steve Reich - Music for 18 Musicians (1976) [FLAC]/Music for 18 Musicians.cue"
DSD="$W/src/Art Farmer & Jim Hall - Big Blues (1978) [DSF] {DD  DSD64}"
run() { name=$1; shift; mkdir -p "$W/out/$name"; echo "### $name start $(date +%T)"; start=$(date +%s.%N); TONEPOET_PIPELINE_BASELINE_JSONL=$W/jsonl/$name.jsonl "$T" convert "$@" --output "$W/out/$name" --workers 4 --replaygain album --write-log 2>&1 | tail -3; echo "### $name wall $(echo "$(date +%s.%N) - $start" | bc)s"; }
run flac3_to_flac "${FLAC3[@]}" --format flac
run flac3_to_opus "${FLAC3[@]}" --format opus
run flac3_to_aac  "${FLAC3[@]}" --format aac
run cue_to_flac   "$CUE" --format flac
run dsd_to_flac   "$DSD" --format flac --dsd-gain true-peak-normalize --dsd-true-peak-scope album --dsd-true-peak-scan standard
echo "### ALL DONE $(date +%T)"
