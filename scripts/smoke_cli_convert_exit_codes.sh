#!/usr/bin/env bash
# Issue #67: black-box exit status smoke. Requires a built tonepoet binary and
# the normal conversion runtime/tools; does not change the checkout.
set -euo pipefail

fail() { printf 'Issue #67 smoke FAILED: %s\n' "$*" >&2; exit 1; }
[[ $# == 1 ]] || fail 'usage: scripts/smoke_cli_convert_exit_codes.sh /absolute/path/to/tonepoet'
[[ -x $1 ]] || fail "binary is missing or not executable: $1"
command -v python3 >/dev/null 2>&1 || fail 'python3 is required to create a real WAV fixture'

binary="$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"
work="$(mktemp -d "${TMPDIR:-/tmp}/tonepoet-cli-exit.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/inputs" "$work/positive" "$work/negative" "$work/mixed" \
         "$work/home" "$work/config" "$work/data" "$work/cache"
# Prevent this smoke from touching the user's configuration, queue or caches.
export HOME="$work/home" XDG_CONFIG_HOME="$work/config" \
       XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"

python3 - "$work/inputs/valid.wav" "$work/inputs/corrupt.flac" <<'PY'
import math
import struct
import sys
import wave

with wave.open(sys.argv[1], 'wb') as stream:
    stream.setnchannels(1)
    stream.setsampwidth(2)
    stream.setframerate(44100)
    frames = (int(3000 * math.sin(2.0 * math.pi * 440 * n / 44100))
              for n in range(44100))
    stream.writeframes(b''.join(struct.pack('<h', sample) for sample in frames))
with open(sys.argv[2], 'wb') as stream:
    stream.write(b'fLaC' + b'not a valid FLAC stream')
PY

run_case() {
    label=$1
    expected=$2
    summary=$3
    shift 3
    status=0
    "$binary" convert "$@" --format flac --workers 1 --no-cue \
        --output "$work/$label" >"$work/$label.log" 2>&1 || status=$?
    if [[ $expected == success && $status -ne 0 ]] || \
       [[ $expected == failure && $status -eq 0 ]]; then
        cat "$work/$label.log" >&2
        fail "$label: expected $expected, actual exit $status"
    fi
    grep -Fq "$summary" "$work/$label.log" || {
        cat "$work/$label.log" >&2
        fail "$label: expected queued-item summary '$summary'"
    }
    printf 'PASS %-8s exit %s (%s)\n' "$label" "$status" "$summary"
}

run_case positive success '1/1 succeeded, 0 failed' "$work/inputs/valid.wav"
run_case negative failure '0/1 succeeded, 1 failed' "$work/inputs/corrupt.flac"
run_case mixed failure '1/2 succeeded, 1 failed' \
    "$work/inputs/valid.wav" "$work/inputs/corrupt.flac"

[[ $(find "$work/positive" -type f -name '*.flac' | wc -l) -eq 1 ]] || \
    fail 'positive conversion did not produce exactly one FLAC'
[[ $(find "$work/negative" -type f -name '*.flac' | wc -l) -eq 0 ]] || \
    fail 'failed conversion unexpectedly published a FLAC'
[[ $(find "$work/mixed" -type f -name '*.flac' | wc -l) -eq 1 ]] || \
    fail 'mixed batch did not publish exactly one successful FLAC'
printf 'Issue #67 CLI exit smoke: all cases passed.\n'
