#!/usr/bin/env bash
# R20 black-box album publication acceptance. Requires a built TonePoet binary
# and a complete configured real conversion toolchain; never alters user data.
set -euo pipefail

fail() { printf 'R20 album publication smoke FAILED: %s\n' "$*" >&2; exit 1; }
[[ $# == 1 ]] || fail 'usage: scripts/smoke_r20_album_publication.sh /absolute/path/to/tonepoet'
[[ -x $1 ]] || fail "binary is not executable: $1"
command -v python3 >/dev/null 2>&1 || fail 'python3 is required for WAV fixtures'
binary="$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"
work="$(mktemp -d "${TMPDIR:-/tmp}/tonepoet-r20-publication.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/home" "$work/config" "$work/data" "$work/cache"
export HOME="$work/home" XDG_CONFIG_HOME="$work/config" \
       XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"

make_wavs() {
    local folder=$1 count=$2
    mkdir -p "$folder"
    python3 - "$folder" "$count" <<'PY'
import math
from pathlib import Path
import struct
import sys
import wave
root = Path(sys.argv[1]); count = int(sys.argv[2])
for index in range(1, count + 1):
    path = root / f'{index:02} - Track {index}.wav'
    with wave.open(str(path), 'wb') as wav:
        wav.setnchannels(1); wav.setsampwidth(2); wav.setframerate(44100)
        samples = (int(2500 * math.sin(2.0 * math.pi * (220 + 70 * index) * frame / 44100))
                   for frame in range(11025))
        wav.writeframes(b''.join(struct.pack('<h', sample) for sample in samples))
PY
}

# Pin the album directory independently of the metadata-free WAV filenames.
# Count direct album children as well as the complete output tree: a recursive
# total alone can pass when each input publishes to a different album folder.
assert_one_album() {
    local output=$1 album=$2 expected=$3 label=$4
    [[ -d $album ]] || fail "$label: expected album directory missing: $album"
    [[ $(find "$album" -maxdepth 1 -type f -iname '*.flac' | wc -l) -eq $expected ]] || \
        fail "$label: expected $expected FLACs directly in $album"
    [[ $(find "$output" -type f -iname '*.flac' | wc -l) -eq $expected ]] || \
        fail "$label: album tracks missing or published to other directories"
}

run_convert() {
    local label=$1; shift
    local status=0
    "$binary" convert "$@" --format flac --workers 2 --no-cue \
        >"$work/$label.stdout" 2>&1 || status=$?
    printf '%s exit %s\n' "$label" "$status"
    printf '%s' "$status"
}

for count in 1 2 3; do
    input="$work/source-$count/R20 Album $count"
    output="$work/output-$count"
    album_name="R20 Album $count"
    album="$output/$album_name"
    make_wavs "$input" "$count"
    mkdir -p "$output"
    status=0
    "$binary" convert "$input" --format flac --output "$output" \
        --folder-naming "$album_name" --replaygain off --write-log --workers 2 --no-cue \
        >"$work/initial-$count.log" 2>&1 || status=$?
    if [[ $status -ne 0 ]]; then cat "$work/initial-$count.log" >&2; fail "first pass N=$count failed"; fi
    assert_one_album "$output" "$album" "$count" "first pass N=$count"
    status=0
    "$binary" convert "$input" --format flac --output "$output" \
        --folder-naming "$album_name" --replaygain off --write-log --workers 2 --no-cue --overwrite-output \
        >"$work/overwrite-$count.log" 2>&1 || status=$?
    if [[ $status -ne 0 ]]; then cat "$work/overwrite-$count.log" >&2; fail "overwrite N=$count failed"; fi
    grep -Fq "$count/$count succeeded, 0 failed" "$work/overwrite-$count.log" || \
        fail "overwrite N=$count omitted successful job summary"
    assert_one_album "$output" "$album" "$count" "overwrite N=$count"
    [[ -z $(find "$output" -type d -name '.tonepoet-backup-*' -print -quit) ]] || \
        fail "successful overwrite N=$count left a recovery backup"
    log_count=$(find "$output" -type f -name 'conversion-*.log' | wc -l)
    [[ $log_count -ge 2 ]] || fail "N=$count: a second run did not retain both conversion logs ($log_count)"
    printf 'PASS overwrite N=%s: all audio survives, no backup remains, history retained\n' "$count"
done

# A subsequent selection of ONE source file is not a shared album batch.
# Without --write-log, this must still replace only that audio file, never
# publish a one-track directory over the previously complete album.
output="$work/output-3"
input="$work/source-3/R20 Album 3"
album_name='R20 Album 3'
album="$output/$album_name"
assert_one_album "$output" "$album" 3 'single-file redo fixture'
mapfile -d '' -t second < <(find "$album" -maxdepth 1 -type f -name '02*.flac' -print0)
mapfile -d '' -t third < <(find "$album" -maxdepth 1 -type f -name '03*.flac' -print0)
[[ ${#second[@]} -eq 1 && ${#third[@]} -eq 1 ]] || \
    fail 'single-file overwrite fixture lacks exactly one second and third sibling in the album'
before_second=$(sha256sum "${second[0]}" | cut -d ' ' -f 1)
before_third=$(sha256sum "${third[0]}" | cut -d ' ' -f 1)
history_before=$(find "$album" -maxdepth 1 -type f -name 'conversion-*.log' | wc -l)
visible_log="$album/conversion.log"
[[ -f $visible_log ]] || fail 'the initial logged album has no visible conversion.log'
visible_log_before=$(sha256sum "$visible_log" | cut -d ' ' -f 1)
status=0
"$binary" convert "$input/01 - Track 1.wav" --format flac --output "$output" \
    --folder-naming "$album_name" --replaygain off --workers 1 --no-cue --overwrite-output \
    >"$work/no-log-one-file-overwrite.log" 2>&1 || status=$?
if [[ $status -ne 0 ]]; then
    cat "$work/no-log-one-file-overwrite.log" >&2
    fail 'unlogged single-file overwrite failed'
fi
assert_one_album "$output" "$album" 3 'unlogged single-file overwrite'
[[ $(sha256sum "${second[0]}" | cut -d ' ' -f 1) == "$before_second" ]] || \
    fail 'unlogged overwrite changed the second track'
[[ $(sha256sum "${third[0]}" | cut -d ' ' -f 1) == "$before_third" ]] || \
    fail 'unlogged overwrite changed the third track'
[[ $(find "$album" -maxdepth 1 -type f -name 'conversion-*.log' | wc -l) -eq $history_before ]] || \
    fail 'a logging-disabled conversion wrote unexpected history'
[[ $(sha256sum "$visible_log" | cut -d ' ' -f 1) == "$visible_log_before" ]] || \
    fail 'an unlogged conversion modified the previous visible log'
[[ -z $(find "$output" -type d -name '.tonepoet-backup-*' -print -quit) ]] || \
    fail 'unlogged single-file overwrite left a whole-album backup'
printf 'PASS unlogged single-file overwrite: both unselected tracks and log history survive\n'

# Keep both must choose one album sibling, never one numbered folder per item.
output="$work/output-3"
input="$work/source-3/R20 Album 3"
album_name='R20 Album 3'
album="$output/$album_name"
status=0
"$binary" convert "$input" --format flac --output "$output" \
    --folder-naming "$album_name" --replaygain off --write-log --workers 2 --no-cue --if-exists keep-both \
    >"$work/keep-both.log" 2>&1 || status=$?
if [[ $status -ne 0 ]]; then cat "$work/keep-both.log" >&2; fail 'keep-both conversion failed'; fi
numbered="$output/$album_name (2)"
[[ -d $numbered ]] || fail 'keep both did not publish the one expected numbered album sibling'
[[ $(find "$album" -maxdepth 1 -type f -iname '*.flac' | wc -l) -eq 3 ]] || \
    fail 'keep both changed the original album track set'
[[ $(find "$numbered" -maxdepth 1 -type f -iname '*.flac' | wc -l) -eq 3 ]] || \
    fail 'keep both did not publish three tracks together in the numbered album'
[[ $(find "$output" -type f -iname '*.flac' | wc -l) -eq 6 ]] || \
    fail 'keep both split tracks into unexpected folders or lost original audio'
printf 'PASS keep-both: one numbered sibling contains all three tracks\n'

# Reproduce the partial ReplayGain cohort: same physical album directory,
# one clean member and one invalid FLAC. Failed input must not erase survivor.
source="$work/reduced-input/R20 Reduced Album"
output="$work/reduced-output"
make_wavs "$source" 1
printf 'fLaCnot a valid FLAC stream\n' > "$source/02 - Damaged.flac"
mkdir -p "$output"
status=0
"$binary" convert "$source" --format flac --output "$output" \
    --replaygain both --write-log --workers 2 --no-cue \
    >"$work/reduced.log" 2>&1 || status=$?
[[ $status -ne 0 ]] || fail 'damaged ReplayGain member incorrectly returned success'
[[ $(find "$output" -type f -iname '*.flac' | wc -l) -eq 1 ]] || {
    cat "$work/reduced.log" >&2
    fail 'damaged ReplayGain member suppressed surviving audio'
}
log="$(find "$output" -type f -name 'conversion.log' -print -quit)"
[[ -n $log && -f $log ]] || fail 'reduced-cohort destination lacks conversion.log'
grep -Fq 'Contributing files: 1' "$log" || fail 'log lacks one-contributor ReplayGain cohort disclosure'
grep -Fq 'Damaged.flac' "$log" || fail 'log fails to identify excluded input'
grep -Fq 'Reason:' "$log" || fail 'log fails to identify exclusion reason'
if command -v metaflac >/dev/null 2>&1; then
    flac="$(find "$output" -type f -iname '*.flac' -print -quit)"
    [[ -n "$(metaflac --show-tag=REPLAYGAIN_ALBUM_GAIN "$flac")" ]] || \
        fail 'surviving FLAC lacks ReplayGain album tags'
fi
printf 'PASS ReplayGain: surviving audio, reduced album tags and exclusion disclosure\n'
printf 'R20 black-box publication smoke: all scenarios passed.\n'
