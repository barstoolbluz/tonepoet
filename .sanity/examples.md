# examples — sanity assessment

30 of 30 read · 9 surprising

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

## examples/ctdb_diag.rs

### the file itself
- spec 3 · read at `8c6c41d27d12` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:32Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A standalone example binary: main parses FLAC file paths from CLI args, decode_flac_to_i16 decodes each to raw i16 PCM samples, compute_suffix_skip figures out how many trailing samples to exclude (matching CTDB's edge-track trimming convention), and compute_track_crc32 runs the CTDB-compatible CRC32 over the remaining samples. The program prints each track's computed CRC32 to stdout so the user can manually diff it against a canonical trackcrcs value pasted from CTDB.
- found: Matches the gist, but decode_flac_to_i16 shells out to the ffmpeg CLI binary rather than using a Rust FLAC decoding library, and the expected CRC values are hardcoded into the script itself rather than pasted/compared interactively as the doc comment implies.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `compute_suffix_skip` — OBSCURE
- spec 3 · read at `43fbb1cdaf7c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:38:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes how many trailing samples to skip when calculating the CTDB CRC32 (CUETools DB CRCs conventionally skip a fixed number of samples near track boundaries), likely returning total_samples.min(some fixed constant like 5*588) as a sample count to exclude from the end.
- found: Converts total_samples to a word count (samples*2, presumably stereo interleaved i16 words), then computes STRIDE_WORDS plus the remainder of that word count modulo STRIDE_WORDS — an alignment-based skip tied to some CRC stride constant, not a fixed sample-boundary skip as I guessed.
- predicted: none · documented: none · derivable: yes · legible: most · trap: no
- note: STRIDE_WORDS is defined elsewhere in the file and not visible here, so the exact rationale for this alignment formula is not derivable from this function alone.

### `compute_track_crc32`
- spec 3 · read at `f53d1d13da69` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:03:54Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes a CTDB-style CRC32 over the track's i16 PCM samples, trimming samples at the boundaries: for the first track it may skip some leading samples, and for the last track it uses suffix_skip_i16 to skip trailing samples (e.g. excluding pregap/postgap silence), then runs a CRC32 (likely via the crc crate or a hand-rolled table) over the remaining sample bytes and returns the checksum.
- found: Trims a fixed PREFIX_SKIP_I16 leading samples on the first track and suffix_skip_i16 trailing samples on the last track, then computes crc32fast::hash over the trimmed i16 samples reinterpreted as bytes via unsafe raw-parts cast; returns 0 if the trim leaves nothing.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `decode_flac_to_i16` — QUIRKY
- spec 3 · read at `550c3d7c25cd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:53:27Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Opens the FLAC file at `path` with a FLAC decoder (likely claxon), iterates through all frames, and flattens/interleaves the decoded per-channel samples into a single Vec<i16>, unwrapping/panicking on errors since this is throwaway diagnostic code.
- found: Shells out to the ffmpeg binary via Command to decode the input file to raw s16le PCM stereo at 44100Hz, panics on failure (including if ffmpeg itself isn't found), then converts the raw stdout bytes into a Vec<i16> by reading little-endian pairs.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `main`
- spec 3 · read at `9b63197b51c5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:48:43Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Parses CLI args as a list of FLAC file paths, decodes each via decode_flac_to_i16, computes a CTDB-style CRC32 per track using compute_track_crc32 (using compute_suffix_skip to handle CTDB's special first/last track sample-skip rules), and prints each track's computed CRC32 to stdout so the user can compare it against a pasted canonical trackcrcs value.
- found: Decodes each given FLAC to i16 samples, computes total_disc_samples and suffix_skip, then computes per-track CRC32 via compute_track_crc32 and compares against a hardcoded canonical CRC array, printing a ✓/✗ match indicator per track — not requiring the user to paste anything externally as the doc comment suggested.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## examples/ctdb_disc_syndrome_probe.rs

### the file itself
- spec 3 · read at `5cbaff89b0e3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:35Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Diagnostic CLI example that probes a disc rip against CTDB (CUETools Database) syndrome data: infers the TOC (infer_ctdb_toc), lets the user pick a matching CTDB entry (select_entry), decodes tracks to raw i16 PCM (accuraterip_decode, accuraterip_decode_track_to_raw_i16), gathers per-track sample counts and candidate TOC offsets to test alignment (accuraterip_collect_sample_counts, accuraterip_find_toc_offsets), and reports whether computed checksums/syndrome match the CTDB entry, with main/run/print_usage forming a standard CLI wrapper.
- found: Matches prediction: CLI parses --toc/--entry-id/--confidence/--all plus track paths, infers TOC via infer_ctdb_toc if not given, queries CTDB, selects an entry, decodes tracks to i16 PCM and assembles a padded image, computes maxNpar=16 CTDB parity, then probes candidate pressing offsets against the entry via ctdb_probe_entry_offsets_with_parity, reporting exact-zero syndrome hits and Chien-search-decodable error counts per offset.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no
- note: The actual Reed-Solomon-style syndrome/parity/Chien-search machinery lives behind ctdb::compute_audio_parity16 and ctdb_probe_entry_offsets_with_parity in the tui::ctdb module, not in this example file — this file is just the CLI harness around it.

### `main`
- spec 3 · read at `47fb02453730` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:11:23Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: The example's entry point: it collects CLI args, and if they don't parse (missing args) calls print_usage() and exits; otherwise it awaits run() with the parsed args, printing any error to stderr and exiting with a nonzero status on failure.
- found: Awaits run() (which presumably parses args itself); on error prints the error, then usage, then exits with code 2.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Args parsing happens inside run(), not main — main is just the error/usage/exit wrapper.

### `run` — QUIRKY
- spec 3 · read at `b48e2c906d0d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:08:12Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: The example's async entry point: parses CLI args (e.g. disc/cue path, maybe a CTDB entry selector), printing usage and returning an error if arguments are missing/invalid. It then infers the CTDB TOC from the disc, selects the relevant entry via select_entry, decodes tracks to raw samples using the accuraterip_decode helpers, computes AccurateRip/CTDB checksums and TOC offsets, and prints diagnostic output about how well the disc matches the CTDB syndrome/offsets — essentially a debugging tool for offset detection.
- found: Parses CLI flags (--toc, --entry-id, --confidence, --all, track paths), infers/queries CTDB TOC and picks a response entry, decodes all tracks into one concatenated i16 sample image padded by STRIDE on each side, computes CTDB parity (maxNpar=16), then probes candidate offsets against the entry's syndrome (exact-zero matches and Chien-search error correction), printing a per-offset diagnostic table and a summary of hit counts and the best error count/offset.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `infer_ctdb_toc` — QUIRKY
- spec 3 · read at `bcc6c2695b59` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:04:00Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Given a list of track file paths, reads each track's sample/frame count (likely via accuraterip_collect_sample_counts or similar), computes cumulative CD frame offsets, and formats them into a CTDB-style TOC string (offsets plus total length), returning Err(String) if any file can't be read or decoded.
- found: Tries to find real disc TOC sector offsets in the first track's directory (preferring authoritative CD TOC data when the count matches track count), and only if that's unavailable falls back to inferring TOC purely from decoded sample counts per track.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `select_entry`
- spec 3 · read at `d109d171794e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:03:04Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Filters `entries` by the optional entry_id and/or confidence values when provided, returning an error string if no entry matches. If neither filter is given and there are multiple entries, it likely picks the one with the highest confidence, or errors out asking the user to disambiguate if there's more than one candidate remaining after filtering.
- found: If entry_id given, finds by id or errors. Else if confidence given, filters by it, erroring if none match or if more than one matches (ambiguous, suggesting --entry-id). Else picks the entry with max confidence, or errors if entries is empty.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `print_usage`
- spec 3 · read at `c1ddaa4c6e47` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:17:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Prints a short "Usage: ctdb_disc_syndrome_probe <args>" style help message to stderr, describing the expected command-line arguments (likely a disc image or TOC path and maybe an AccurateRip/CTDB related option), called when args are missing or invalid.
- found: Eprintln's a one-line usage string showing the CLI flags: --toc, --entry-id/--confidence, --all, and a TRACK... list.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `accuraterip_decode`
- spec 3 · read at `bc6e55042f08` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:26:44Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Calls a tonepoet library function to decode the audio file at path into raw samples, mapping any error to a String via .map_err(|e| e.to_string()), and returning the Vec<i16> samples.
- found: Directly delegates to accuraterip_decode_track_to_raw_i16(path) with no additional logic — a one-line pass-through wrapper.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `accuraterip_decode_track_to_raw_i16`
- spec 3 · read at `04b0d94de6fa` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:31:52Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls the peer function accuraterip_decode (or similar decoder) on the file at `path`, converts the resulting audio buffer into a flat Vec<i16> of raw samples, and maps any error into a String for this example binary's error handling.
- found: One-line delegation to the accuraterip crate's decode_track_to_raw_i16(path) function.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `accuraterip_collect_sample_counts` — QUIRKY
- spec 3 · read at `0315e1438c4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:36:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: For each path in `paths`, it opens/decodes the audio track (likely via accuraterip_decode or similar) to determine its sample count, collecting these into a Vec<u64> in track order. The u32 in the return tuple is probably a shared sample rate or channel count read from the first track, and it returns an Err(String) if any track fails to decode or has an unexpected format.
- found: One-line thin wrapper that just forwards to accuraterip::collect_sample_counts(paths); the actual logic lives in the accuraterip crate/module, not here.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `accuraterip_find_toc_offsets`
- spec 3 · read at `eadf4a123ce7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:41:52Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Delegates to an existing accuraterip/toc parsing utility elsewhere in the crate to locate a cue/log file in `dir` and extract track offsets, returning None if no such file exists or parsing fails.
- found: One-line wrapper delegating directly to accuraterip::find_toc_offsets(dir), presumably just a local rename/wrapper so the example file has a locally-namespaced function name.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## examples/ctdb_rs_verify.rs

### the file itself — QUIRKY
- spec 3 · read at `b37d1232cb11` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:38Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A single-`main` example binary: reads track file paths from CLI args, builds a TOC from them, queries the CTDB (CUETools DB) service for matching disc entries, decodes each track and runs Reed-Solomon-based verification against the matched entry plus a per-track CRC check, then prints per-track pass/fail results and the confidence score of the matched CTDB entry to stdout.
- found: CLI example: takes track paths, probes sample counts/rate, opens the persistent tonepoet SQLite db to check/reuse a cached CTDB parity blob (keyed by track content) to skip a ~10s parity computation, runs ctdb::verify_ctdb on a tokio runtime, stores freshly computed parity back to the cache, then prints the TOC, matched-entry npar/stride/parity_url, per-track status/CRC/confidence, and a summary line.
- predicted: some · documented: none · derivable: yes · legible: not judged · trap: no

### `main`
- spec 3 · read at `2d9c5cacb77c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:45:17Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Parses CLI args as a list of track file paths, builds a TOC from them, queries CTDB (CUETools DB) for matching entries, decodes the tracks, runs Reed-Solomon verification, computes per-track CRCs, and prints results per track along with the confidence of the best-matched CTDB entry — exiting with a nonzero status or printing an error/usage message if no tracks are given or something fails along the way.
- found: Parses track paths from argv, collects sample counts/rate, opens the tonepoet SQLite DB and checks/uses a persistent parity cache keyed off the tracks, runs ctdb::verify_ctdb on a tokio runtime, stores freshly-computed parity back to the cache, then prints TOC, matched entry info, per-track status/CRC/confidence, and a final formatted summary.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: Missed the parity-cache read/write plumbing (SQLite-backed, tied to a computed cache key) entirely — the docs and signature give no hint that this example exercises caching behavior shared with the TUI's event loop.

## examples/ctdb_syndrome_probe.rs

### the file itself — QUIRKY
- spec 3 · read at `624d8a7fd65d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:48Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: main parses 4 track paths from argv, decodes each FLAC to i16 samples via decode_flac_to_i16, computes the CTDB parity/syndrome using the existing parity_to_syndrome_with_word_offset helper at a range of small offsets, and prints a comparison of the computed syndrome bytes at offset 0 against the canonical 896-entry's known syndrome bytes, reporting whether they match — a standalone debug binary, not part of the library.
- found: decode_flac_to_i16 shells out to ffmpeg to get raw s16le PCM. main assembles a disc image (STRIDE leadin + 4 tracks + STRIDE leadout), computes the parity matrix via ctdb_rs::syndrome::compute_parity_matrix_from_audio, then tests four separate hypotheses against a hardcoded canonical syndrome value: (1) small-offset LE syndrome match via manual GF exponent-table math mirroring parity_to_syndrome_with_word_offset, (2) same in big-endian, (3) whether the syndrome bytes are literally a raw parity row, (4) whether the syndrome bytes are a column slice, and finally (5) whether XORing our parity row against the entry's syndrome and Berlekamp-Massey decoding the delta yields a correctable result — printing diagnostics for each.
- predicted: some · documented: most · derivable: no · legible: not judged · trap: no
- note: The doc header undersells the file — it describes only the first hypothesis (offset-trial + row-0 compare) but the file actually walks through five distinct hypotheses (LE, BE, parity-row, column, BM-decode) with ~300 lines of exploratory probing.

### `decode_flac_to_i16` — QUIRKY
- spec 3 · read at `39decdaf25fc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:24:05Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Opens the FLAC file at `path`, decodes it fully using a FLAC decoder crate, and collects all decoded samples into a Vec<i16> (interleaved if multi-channel), unwrapping/panicking on any I/O or decode error since this is a quick diagnostic example rather than production code.
- found: Shells out to the ffmpeg CLI binary to decode the file at path into raw signed 16-bit little-endian PCM at 44100Hz stereo on stdout, panicking on spawn failure or nonzero exit status, then converts the raw stdout bytes into a Vec<i16> via chunks_exact(2) and from_le_bytes.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Uses an external ffmpeg subprocess rather than an in-process FLAC decoding crate, which isn't guessable from the function name alone.

### `main` — QUIRKY — TANGLED
- spec 3 · read at `ed65476a8fc9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:09Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A CLI diagnostic tool: parses 4 track file path args, decodes each FLAC to i16 samples via decode_flac_to_i16, computes parity/syndrome using parity_to_syndrome_with_word_offset at a range of small offsets, and prints/compares the resulting syndrome bytes against a hardcoded canonical "896 entry" syndrome to find which offset (if any) matches, printing diagnostics per offset.
- found: Decodes 4 tracks to i16, assembles a disc image with lead-in/out padding, computes the CTDB parity matrix, then tests multiple hypotheses in sequence to find how our computed data relates to a canonical '896 entry' target syndrome: (1) syndrome-from-parity-row at small offsets in LE, (2) same in BE, (3) target bytes are a raw parity row, (4) target bytes are a parity column slice, (5) target is entry's parity[0] and XOR-ing with our row then Berlekamp-Massey decoding recovers a correctable error count — printing diagnostics and an early return on any exact match.
- predicted: some · documented: most · derivable: no · legible: some · trap: no
- note: The file_doc undersells this — main() explores five distinct hypotheses (byte order, direct row match, column match, BM-decode) well beyond the described single offset-trial comparison.

## examples/dump_sacd_lsn.rs

### the file itself
- spec 3 · read at `f118b2dfd29b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:43Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A small example binary: main takes an ISO path from CLI args, opens it with this repo's SACD parsing code, iterates over the disc's tracks/areas, and prints each track's start_lsn/end_lsn range to stdout, so those values can be copied into a byte-comparison test harness against the sacd-rs crate.
- found: Matches core prediction: parses an ISO via tonepoet's parse_sacd_iso and prints per-track LSN ranges, but iterates both stereo and multi_channel areas separately and also prints header info (channel_count, dst_encoded, track_count) and start/duration timecodes per track.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `main`
- spec 3 · read at `5e16e2f24133` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:34Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Reads the ISO path from argv, opens/parses it as a SACD image, iterates over its tracks (likely stereo and/or multichannel area), and prints each track's index alongside its start_lsn and end_lsn to stdout in a simple, easily-copyable format for use as reference values in the sacd-rs byte-comparison test harness. Exits with an error message if the path argument is missing or the ISO fails to parse.
- found: Parses the ISO path arg, parses the SACD image, and for each present area (stereo/multi_channel) prints area header info (channel count, DST encoding, track count, area TOC LSN range) followed by each track's start/end LSN, sector length, and start-time/duration in minutes:seconds:frames.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

## examples/key_event_probe.rs

### the file itself
- spec 3 · read at `b1db6b9c40df` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:49Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: RawModeGuard is an RAII wrapper enabling terminal raw mode on enter() and restoring normal mode in drop(); read_press blocks reading one crossterm KeyEvent; report prints its fields; is_plain_backspace and is_supported_strong_delete_delivery classify a captured event to distinguish plain Backspace from Ctrl+Backspace (checking modifiers/keycode, possibly against known terminal-specific encodings); main sets up the guard, prompts for the two key presses in sequence, reads and reports each.
- found: Matches prediction closely: RawModeGuard RAII enter/drop, read_press loops until a Press-kind key event (with an explicit Ctrl+C escape hatch since raw mode delivers it as a key event not a signal), report prints code/modifiers/kind/state, is_plain_backspace checks Backspace without Ctrl/Alt, is_supported_strong_delete_delivery accepts either Ctrl+Backspace or a Ctrl+H fallback (baseline terminal encoding). main reads both presses, restores cooked mode, prints both reports, and exits with an error if the delivery isn't in the recognized compatible set.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `enter`
- spec 3 · read at `7d3e6adbb245` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:26:43Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Enables terminal raw mode via crossterm::terminal::enable_raw_mode(), then returns Ok(RawModeGuard) as a unit/marker struct so that its Drop impl later disables raw mode, implementing the RAII pattern described in the file doc.
- found: Calls enable_raw_mode() and returns Ok(Self), exactly as predicted.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `drop`
- spec 3 · read at `75cd0ce3437f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:31:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Drop impl for RawModeGuard that calls crossterm::terminal::disable_raw_mode() to restore normal terminal mode, ignoring or discarding any resulting error since Drop can't propagate one.
- found: Calls disable_raw_mode() and discards the result, restoring the terminal on drop.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `read_press`
- spec 3 · read at `5b39b163a373` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:52:05Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Prints a prompt using label (e.g. "Press plain Backspace"), then loops calling crossterm's event::read() until it receives an Event::Key with KeyEventKind::Press (skipping release/repeat events), and returns that KeyEvent.
- found: Prints "Press {label} once.", loops reading crossterm events, skips non-Press key kinds, and specifically checks for Ctrl+C to return an Interrupted error as an escape hatch (since raw mode delivers it as a key event, not a signal), otherwise returns the pressed key.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: The Ctrl+C escape hatch is explained by an inline comment, which is why derivable is false and documented counts as most despite the file_doc being about the example's purpose rather than this function specifically.

### `report`
- spec 3 · read at `9f3489ff1ab3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:44Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Prints (via println!) a line combining the label with the KeyEvent's code, modifiers, and kind fields in debug format, so the user running the probe can see exactly what crossterm delivered for that keypress.
- found: Prints label plus key.code, modifiers, kind, and state in debug format.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `is_plain_backspace`
- spec 3 · read at `f63f3d2fa49e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:32:56Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Returns true if key.code == KeyCode::Backspace and key.modifiers is empty (KeyModifiers::NONE), i.e. a plain unmodified Backspace press with no Ctrl/Alt/Shift.
- found: Checks key.code is Backspace and modifiers lack CONTROL and ALT specifically (not a full check against NONE, so SHIFT would still pass) — I predicted a stricter "modifiers == NONE" check.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: No file-level doc attaches to this specific function; the module doc describes the whole example program, not this helper's exact modifier logic.

### `is_supported_strong_delete_delivery`
- spec 3 · read at `5d37f4c9f4f8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:31Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given KeyEvent matches one of the known terminal encodings for Ctrl+Backspace ("strong delete") — e.g. KeyCode::Backspace with CONTROL modifiers, or a control character code like 0x08/0x7f — returning true if recognized, false otherwise.
- found: Returns true if the key is Ctrl+Backspace or the baseline Ctrl+H fallback, in both cases explicitly requiring ALT not be held (to avoid mistaking Alt-combinations for these).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `main`
- spec 3 · read at `25e88b1e17ec` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T06:01:35Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Enters raw mode via RawModeGuard, prints instructions to the user, then calls read_press twice (once for plain Backspace, once for Ctrl+Backspace), printing each captured KeyEvent via report(), possibly annotating with is_plain_backspace/is_supported_strong_delete_delivery checks. Returns Ok(()) with the guard's Drop restoring terminal state.
- found: Enters raw mode, reads plain Backspace then Ctrl+Backspace via read_press, drops raw mode, prints both reports, then validates the captured events against expected shapes (is_plain_backspace / is_supported_strong_delete_delivery) and returns Ok or an InvalidData error depending on whether delivery matched expectations.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
