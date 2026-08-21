# scripts — sanity assessment

22 of 22 read · 10 surprising

Each entry below is one **reading**, of a function or of a whole file. An
agent was given its name, signature, neighboring names and comments — never
its body — and wrote down what it expected to find. Then it opened the file.
The gap between the two is the finding. A file's own entry is titled `the file
itself` and asks whether the header at the top describes what is actually in
there.

`read at` is a hash of the body as it was when the reading was made. When it
stops matching the code, the reading is marked STALE and goes back in the
queue.

What this is and how to add to it: [README.md](README.md)

## scripts/dvda_corpus_probe.py

### the file itself
- spec 3 · read at `7bbfc9fabf72` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:55Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A standalone Python diagnostic script (not part of the Rust build) that binary-parses the three DVD-Audio IFO file types — AUDIO_TS.IFO (AMG, parse_amg), ATS_XX_0.IFO (ATSI, parse_atsi), and AUDIO_PP.IFO (SAMG, parse_samg) — using struct offsets ported from foo_input_dvda's C++ source; decode_sample_rate/decode_bit_depth/parse_audio_format decode the packed audio-format byte(s) shared across those formats. probe_disc runs all three parsers against one fixture directory and probe/print_human_readable format the results either as human-readable text or JSON; main handles CLI args to run this over a single fixture dir or every subdir under a corpus root, to help developers inspect/debug real-world DVD-A disc structures against the Rust decoder's assumptions.
- found: Parses AMG/ATSI/SAMG IFO binary structures (magic, sector pointers, packed sample-rate/bit-depth/channel-assignment fields) plus, for ATSI, full title/track/index tables with PTS timing and sector-pointer-to-track assignment; probe_disc also checks for a DVDAUDIO.MKB (CPPM) file and inventories all files in the fixture dir; main resolves either a single fixture dir or a corpus root of many fixture dirs and prints human-readable or JSON output.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `parse_amg`
- spec 3 · read at `0d6fab245a6a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:09:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads fixed-offset fields from the AUDIO_TS.IFO binary (magic/identifier string, version, number of titlesets, various table offsets/counts) via struct unpacking, returning them as a dict of structural fields, mirroring parse_atsi/parse_samg for their respective IFO files.
- found: Validates the 12-byte magic string "DVDAUDIO-AMG" (returns an error dict if mismatched), then unpacks fixed-offset big-endian fields (last sectors, spec version, category, volume counts, disc side, video/audio title-set counts, provider identifier string) into a dict.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `decode_sample_rate`
- spec 3 · read at `79f7b38e0478` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:09:06Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Maps the 4-bit coded sample-rate field to a table of known DVD-Audio rates (the low nibble typically distinguishing the 48kHz family: 48000/96000/192000 from the 44.1kHz family: 44100/88200/176400), returning the Hz value, with some fallback (0 or an "unknown") for unrecognized codes.
- found: Splits coded value into low 3 bits (index into a 3-entry table, capped) and bit 0x08 selects between 44.1kHz-family and 48kHz-family lookup tables, defaulting to 0 for out-of-range or missing entries. Matched the two-family/table split idea but the exact bit layout (3-bit index vs full nibble, index>2 short-circuit) was more specific than I predicted.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `decode_bit_depth`
- spec 3 · read at `411f7a23953e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:26:44Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Maps a small integer code (from a 4-bit field) to a DVD-Audio bit depth using a lookup table, likely {0: 16, 1: 20, 2: 24}, returning the corresponding bit depth in bits.
- found: Looks up the coded value in a module-level BITDEPTH_TABLE dict, defaulting to 0 if not found; exact table contents (values I guessed) not shown here.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `parse_audio_format` — QUIRKY
- spec 3 · read at `fd9cca7ab5c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:45:20Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Unpacks the 16-byte ats_audio_format entry via struct, pulling out a coding-mode/codec byte, a packed byte containing sample rate and bit depth (decoded through decode_sample_rate/decode_bit_depth), and a channel count, returning them as a dict of named fields for diagnostic printing.
- found: Unpacks a 2-byte audio_type field plus a 3-byte channel_fmt_t bitfield, splitting the bitfield into group1/group2 sample-rate and bit-depth nibble codes (decoded via decode_sample_rate/decode_bit_depth) and a channel_assignment byte looked up in a CHANNEL_ASSIGNMENTS table, returning all of it plus a hex-formatted audio_type as a dict.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `parse_atsi` — QUIRKY — TANGLED
- spec 3 · read at `13e0c8f192d0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:00:02Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Uses struct.unpack at fixed byte offsets (per the DVD-Audio ATSI binary layout derived from foo_input_dvda) to extract fields like number of titles, number of audio/subpicture streams, and table offsets, then iterates the audio attribute table calling parse_audio_format/decode_sample_rate/decode_bit_depth for each stream entry. Returns a dict summarizing these fields (title count, audio streams with sample rate/bit depth/channels, etc.) for the diagnostic tool to print or serialize as JSON.
- found: Validates the "DVDAUDIO-ATS" magic, unpacks fixed header fields via struct.unpack at hardcoded offsets (documented inline with a full byte-offset layout comment), parses 8 audio_format entries via parse_audio_format, then walks the audio_pgcit table at 0x800 to build a nested titles->tracks structure with per-track timestamps and duration, plus sector-pointer tables that are matched back to tracks by index range; returns one large dict with magic/header fields, audio_formats, and titles.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `parse_samg` — QUIRKY
- spec 3 · read at `09ab3b214923` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:20:55Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Reads the SAMG (Simple Audio Manager) header from the given AUDIO_PP.IFO bytes using struct unpacking at fixed offsets, extracting fields like a signature/identifier string, last sector, and various pointer/count fields analogous to parse_amg and parse_atsi, and returns them as a dict for the diagnostic printer to consume.
- found: Validates the DVDAUDIOSAPP magic, reads track count and spec version, then loops over each track entry unpacking group/track numbers, PTS timing, zone, per-group sample rate/bit depth codes (via decode_sample_rate/decode_bit_depth), channel assignment, and absolute sector ranges into a list of track dicts, skipping empty zero-length tracks, returning it all in a summary dict.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The docstring is just a one-line label ("Parse AUDIO_PP.IFO"); it doesn't hint at the per-track field-by-field bitfield decoding, which is the bulk of the function.

### `probe_disc`
- spec 3 · read at `c44e2aabc3dd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:02:31Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Given a fixture directory, this locates AUDIO_TS.IFO and parses it with parse_amg, then finds all ATS_XX_0.IFO files and parses each with parse_atsi, and if AUDIO_PP.IFO exists parses it with parse_samg. It assembles all these parsed structures into a single dict describing the whole disc, returned for either JSON output or human-readable printing.
- found: Parses AUDIO_TS.IFO/AUDIO_PP.IFO/ATS_*_0.IFO if present via parse_amg/parse_samg/parse_atsi, plus checks for DVDAUDIO.MKB (CPPM) presence/size and lists all files in the directory with sizes, assembling everything into one result dict.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `print_human_readable`
- spec 3 · read at `9c58cb23fd27` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:24:01Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Walks the disc dict returned by probe_disc (parsed AMG/ATSI/SAMG structures) and prints a formatted, indented text report: fixture/disc name, zone/titleset counts, and per-track details like sample rate, bit depth, and channel/audio-format info, mirroring the same data a --json dump would emit but as readable text.
- found: Prints a banner with directory name, then AMG header info, CPPM protection status, per-titleset ATSI info (audio formats, titles, tracks with PTS/sector ranges), and SAMG track index info if present — each section handling an 'error' key gracefully.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Missed the CPPM/copy-protection reporting section entirely, and the error-key-per-section handling pattern.

### `main`
- spec 3 · read at `b1ce08b5a9b1` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T06:14:07Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Uses argparse (or manual sys.argv parsing) to get fixture_dir and --json flag, determines whether the given path is a single fixture directory or a parent containing multiple fixture subdirs, then for each calls probe_disc() and either dumps results as JSON or calls print_human_readable().
- found: Manually parses sys.argv for fixture_dir and --json flag, determines single vs multi-fixture by checking for AUDIO_TS.IFO directly in target vs its subdirs, probes each and either prints human-readable per-disc or collects into a JSON array printed at the end.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## scripts/dvda_mlp_fixture_check.py

### the file itself — QUIRKY
- spec 3 · read at `62314110eabc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:58Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A standalone Python script validating DVD-Audio MLP fixture files independent of the Rust test suite. iter_mlp_packets walks the raw MLP stream yielding packet/access-unit boundaries, access_unit_len parses an access-unit header to get its byte length, reassemble stitches parsed access units back into a sample count or raw stream, ffmpeg_sample_count shells out to ffmpeg/ffprobe to get an independent reference sample count for the same fixture, and main compares the two, printing pass/fail so a fixture's expected framing can be sanity-checked without running cargo test.
- found: iter_mlp_packets demuxes DVD-Video-style 2048-byte sectors/PES packets to extract private-stream-1 MLP payloads with first-access-unit pointers; reassemble strips leading fragments/padding and frames access units by length, collecting detailed stats; ffmpeg_sample_count optionally decodes via ffmpeg/ffprobe; main just prints stats and the ffmpeg count rather than doing a diff/pass-fail assertion.
- predicted: some · documented: none · derivable: no · legible: not judged · trap: no

### `iter_mlp_packets` — QUIRKY
- spec 3 · read at `1bd88c7ae26e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:52:20Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A generator that walks through raw AOB byte data, using the sibling access_unit_len helper to determine each MLP access unit's length from its header, and yields successive packet byte slices (or (offset, packet_bytes) tuples) until the buffer is exhausted, so the caller can validate or reassemble frames without needing the Rust test harness.
- found: Iterates over 2048-byte DVD sectors, validates each has a pack header, walks PES packets within the sector, and for private-stream-1 packets whose substream matches the MLP stream ID, yields (sector_index, pointer, payload) tuples extracted from the PES/substream headers.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `access_unit_len`
- spec 3 · read at `35be95516f97` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:15:05Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Parses the MLP access-unit header at buf[offset:] — reads a big-endian 16-bit length field, masks off the top flag bits, multiplies by 2 (since MLP header lengths are given in 16-bit words) to get the byte length of the access unit, and returns None if there aren't enough bytes remaining in buf to read the header.
- found: Reads a big-endian 16-bit field, masks with MLP_LENGTH_MASK, multiplies by 2 to get byte length, then validates the result is within a sane range (>=4 and <= mask*2) before returning it, else returns None; also returns None if not enough bytes remain.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reassemble`
- spec 3 · read at `b6cf0ff3df7d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:26:28Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Takes the sequence of raw packet byte chunks (from iter_mlp_packets) and reassembles them into complete MLP access units by buffering leftover bytes across packet boundaries and using access_unit_len to find each unit's length, yielding/returning the list of complete access-unit byte strings.
- found: Skips the leading fragment before the first access unit using the sector pointer field, then buffers payload bytes across packets and repeatedly extracts complete access units via access_unit_len, tracking all-zero padding runs separately from real data, and returns (framed_bytes, leftover_pending_bytes, stats_dict) rather than just a list of units.
- predicted: most · documented: none · derivable: no · legible: most · trap: no

### `ffmpeg_sample_count`
- spec 3 · read at `042fea80cc4f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:10:52Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes the raw MLP bytes to a temporary file, then shells out to ffprobe/ffmpeg to decode it and parse the reported sample/frame count from its output (e.g. via ffprobe -show_streams or a decode+count invocation). Returns None if ffmpeg isn't installed or the invocation fails/produces unparseable output, so the caller can skip cross-validation gracefully.
- found: Returns None if ffmpeg/ffprobe aren't on PATH; otherwise writes the MLP bytes to a temp file, decodes it via ffmpeg into a PCM WAV file (two-step, not direct MLP probing), then runs ffprobe on the WAV to read duration_ts and returns it as an int.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `main` — QUIRKY
- spec 3 · read at `0950f6d67427` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:54:19Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Takes a fixture path from argv, iterates MLP packets via iter_mlp_packets/reassemble to compute the sample count from the raw bitstream, compares it against ffmpeg_sample_count (running ffmpeg/ffprobe on the same fixture), and prints a pass/fail message, exiting with a nonzero status on mismatch.
- found: Parses a fixture path and optional --ffmpeg flag, reassembles MLP packets from the fixture bytes, prints diagnostic stats (sorted) and leftover carry byte count, and optionally prints an ffmpeg-decoded sample count if --ffmpeg is passed — it's diagnostic output, not an assert/pass-fail check with exit codes.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## scripts/validate_chunk2_1_1_settings_sentinel.sh

### the file itself — OBSCURE
- spec 3 · read at `fe80b094061f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:50:04Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A tiny 4-line shell script (part of a numbered "chunk" validation harness, likely task 2.1.1 of a checklist/migration) that sources a shared helper library defining run_and_record, then calls it once with a specific check for a "settings sentinel" value to confirm that checklist step is done, logging pass/fail.
- found: Defines run_and_record inline (not sourced from elsewhere) to tee each command's output into a timestamped report file under target/, checks cargo is on PATH, then runs a fixed generic sequence — cargo fmt --check, cargo test --workspace, cargo test -p tonepoet-pipeline --all-features, cargo clippy -D warnings — and appends a PASS footer. It doesn't check anything specific to "settings" or a "sentinel" value at all; the name is just this checklist chunk's label, not a description of targeted content.
- predicted: none · documented: none · derivable: yes · legible: not judged · trap: no
- note: The filename implies a targeted settings-sentinel check but the script is actually a generic full-workspace build/test/lint gate with no settings-specific logic — misleading for anyone trying to find what "sentinel" actually means here.

### `run_and_record` — QUIRKY
- spec 3 · read at `01724608b766` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:26:46Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A shell helper that takes a description and command, runs the command, captures its exit status, prints a pass/fail line, and appends the result to an array/counter tracking overall test outcomes for this validation script.
- found: Echoes the command being run and tees output (and the command's own output) to a REPORT log file; no exit-status capture or pass/fail tracking, just logging the command and its combined stdout/stderr.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

## scripts/validate_concurrency_round6.sh

### the file itself — QUIRKY
- spec 3 · read at `524120ae9470` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:43:18Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A shell script (one of a numbered series of "round" validation scripts) that exercises some system under concurrent load — likely launching multiple parallel workers/processes against a shared workspace — and then uses its helper function scan_workspace_log to inspect a log file for signs of race conditions, corruption, or ordering violations. It probably ends by printing a pass/fail verdict based on what the scan finds.
- found: A round-6 acceptance gate script: verifies toolchain tools present, strips RUST_MIN_STACK/TONEPOET_* env vars, runs several python static verifier scripts, runs cargo fmt check, runs a fixed list of ~19 focused regression tests by name, then runs the full workspace test suite 5 times checking each log via scan_workspace_log for 'stack overflow' or a specific lease-staging ENOENT message, then runs one specific test 50 times as a SIGPIPE arbitration stress test, finally printing a pass message.
- predicted: some · documented: none · derivable: yes · legible: not judged · trap: no
- note: The hardcoded test-name list and specific error strings tie this script tightly to a particular historical bug-fix round; it will silently stop testing what it intends to if those test names are renamed/removed.

### `scan_workspace_log`
- spec 3 · read at `195c456d4173` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:56Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Greps a captured workspace/build log file for a set of suspicious patterns (e.g. "panic", "deadlock", "leaked", or specific concurrency-warning strings) relevant to this round-6 concurrency validation, printing any matches and setting a failure flag or exiting non-zero if found.
- found: Checks a log file for two specific failure signatures — a literal "stack overflow" and a "create persistent lease staging file ... No such file or directory" ENOENT pattern — printing to stderr and returning 1 if either is found.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## scripts/validate_concurrency_round7.sh

### the file itself
- spec 3 · read at `524120ae9470` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:43:20Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A shell script validating safe concurrent behavior (likely of a workspace/locking system), by spawning parallel operations against a shared workspace and then checking results. Its one declared function, scan_workspace_log, probably parses a log file to detect race conditions, overlapping writes, or lock violations. "round7" suggests it's part of a series of iterative validation scripts (round1..N), each covering a different concurrency scenario/regression, and having no header docs means intent is only inferable from naming.
- found: A bash acceptance-gate script for a "round7" concurrency fix: it checks required tools, strips forbidden env knobs (RUST_MIN_STACK, TONEPOET_*), runs a chain of prior rounds' static verifier scripts (tools/verify_concurrency_corrective_round*.py) plus mutation/coordination audits, runs cargo fmt check, then runs a long fixed list of specific named focused regression tests one at a time, then runs the full workspace test suite 5 times checking each run's log via scan_workspace_log for stack overflows, a specific lease-staging ENOENT message, or a same-path metadata self-overlap regex match, and finally stress-runs one SIGPIPE-related test 50 times. It's a very specific, narrow acceptance script tied to a particular bug's regression suite, not a general concurrency test harness.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no
- note: The huge list of pinned test names and round-numbered verifier scripts imply a long prior history of "round1..round7" fixes for one gnarly concurrency/metadata bug; scan_workspace_log's checks (stack overflow, lease ENOENT, self-overlap regex) are essentially a fingerprint of the specific failure modes that recurred across those rounds.

### `scan_workspace_log` — QUIRKY
- spec 3 · read at `008dc413f44d` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:57Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Takes a log file (likely a cargo test/build output log), greps it for failure markers such as "panicked", "deadlock", "leak", or "error", prints matching lines for diagnosis, and returns/sets a non-zero status or failure flag if any are found, feeding into the script's overall pass/fail summary for this validation round.
- found: Scans a given log file for three specific known-bad patterns: literal "stack overflow", a regex for lease-staging file ENOENT errors, and (via an embedded Python script) a case where a "filesystem mutation conflicts with live owner" message reports a path overlapping itself — printing a diagnostic to stderr and returning 1 if any is found, else falling through with no explicit return (implicit 0).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The checks are very specific regression signatures (self-overlap bug, ENOENT during lease staging, stack overflow) rather than generic failure-marker scanning, so this only makes sense read against the specific bugs round 7 was chasing.
