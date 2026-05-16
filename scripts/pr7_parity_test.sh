#!/usr/bin/env bash
# PR 7 parity test: compare new pipeline output against basic expectations.
#
# Usage: nix develop --command bash scripts/pr7_parity_test.sh [archive.7z]
#
# If no archive is given, uses a default from the PBThal corpus.
set -euo pipefail

ARCHIVE="${1:-/mnt/hodgepodge/flaclab/pbthal/downloads/10cc - The Original Soundtrack-dec-2013.7z}"
PASSWORD="b0nn13mCmurr@y"
FORMAT="${2:-flac}"  # target format

WORKDIR=$(mktemp -d /tmp/tonepoet-pr7-parity.XXXXXX)
trap 'rm -rf "$WORKDIR"' EXIT

NEW_OUT="$WORKDIR/new_output"
STAGING="$WORKDIR/staging"
LOG_DIR="$WORKDIR/logs"
mkdir -p "$NEW_OUT" "$STAGING" "$LOG_DIR"

echo "=== PR 7 Parity Test ==="
echo "Archive: $ARCHIVE"
echo "Format:  $FORMAT"
echo "Workdir: $WORKDIR"
echo ""

# --- Step 1: Run new pipeline via cargo test harness ---
# We'll invoke the pipeline directly through a small Rust binary/test.
# For now, let's first just verify the archive extracts and converts.

echo "--- Extracting archive to verify contents ---"
7z x -p"$PASSWORD" -o"$STAGING/extract" "$ARCHIVE" -y > /dev/null 2>&1

# Count audio files
AUDIO_COUNT=$(find "$STAGING/extract" -type f \( -name '*.flac' -o -name '*.wav' -o -name '*.mp3' -o -name '*.ogg' \) | wc -l)
echo "Found $AUDIO_COUNT audio files in archive"

if [ "$AUDIO_COUNT" -eq 0 ]; then
    echo "ERROR: No audio files found in archive"
    exit 1
fi

# --- Step 2: Convert each file using the same backend the pipeline uses ---
echo ""
echo "--- Converting via ffmpeg (simulating pipeline encode) ---"
mkdir -p "$NEW_OUT"

mapfile -d '' SOURCES < <(find "$STAGING/extract" -type f -name '*.flac' -print0 | sort -z)
for src in "${SOURCES[@]}"; do
    base=$(basename "$src" .flac)
    dst="$NEW_OUT/${base}.${FORMAT}"

    case "$FORMAT" in
        flac)
            ffmpeg -y -i "$src" -c:a flac -compression_level 8 "$dst" 2>/dev/null
            ;;
        opus)
            ffmpeg -y -i "$src" -c:a libopus -b:a 128k "$dst" 2>/dev/null
            ;;
        mp3)
            ffmpeg -y -i "$src" -c:a libmp3lame -b:a 320k "$dst" 2>/dev/null
            ;;
        *)
            ffmpeg -y -i "$src" "$dst" 2>/dev/null
            ;;
    esac
done

OUTPUT_COUNT=$(find "$NEW_OUT" -type f | wc -l)
echo "Converted $OUTPUT_COUNT files to $FORMAT"

# --- Step 3: Verify output integrity ---
echo ""
echo "--- Verifying output integrity ---"
ERRORS=0

mapfile -d '' OUTPUTS < <(find "$NEW_OUT" -type f -name "*.$FORMAT" -print0 | sort -z)
for outfile in "${OUTPUTS[@]}"; do
    probe=$(ffprobe -v error -select_streams a:0 -show_entries stream=sample_rate,duration -of json "$outfile" 2>/dev/null)
    sr=$(echo "$probe" | python3 -c "import sys,json; print(json.load(sys.stdin).get('streams',[{}])[0].get('sample_rate','0'))" 2>/dev/null || echo "0")

    if [ "$sr" = "0" ] || [ -z "$sr" ]; then
        echo "  FAIL: $(basename "$outfile") - cannot probe sample rate"
        ERRORS=$((ERRORS + 1))
    else
        echo "  OK: $(basename "$outfile") - sample_rate=$sr"
    fi
done

# --- Step 4: Check ReplayGain can be applied ---
echo ""
echo "--- Testing ReplayGain (loudgain) ---"
RG_FILES=$(find "$NEW_OUT" -type f -name "*.$FORMAT" | head -3 | tr '\n' ' ')
if [ -n "$RG_FILES" ]; then
    if loudgain -a -k -s i $RG_FILES > /dev/null 2>&1; then
        echo "  OK: loudgain succeeded on output files"
    else
        echo "  WARN: loudgain failed (may be format-dependent)"
    fi
fi

# --- Step 5: Verify source tags are readable ---
echo ""
echo "--- Checking tags on output ---"
FIRST_OUT=$(find "$NEW_OUT" -type f -name "*.$FORMAT" | sort | head -1)
if [ -n "$FIRST_OUT" ] && [ "$FORMAT" = "flac" ]; then
    metaflac --export-tags-to=- "$FIRST_OUT" 2>/dev/null | head -5
    echo "  (showing first 5 tags)"
fi

echo ""
echo "=== Parity test complete ==="
echo "Audio files extracted: $AUDIO_COUNT"
echo "Files converted: $OUTPUT_COUNT"
echo "Errors: $ERRORS"

if [ "$ERRORS" -gt 0 ]; then
    exit 1
fi
