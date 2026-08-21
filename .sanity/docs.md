# docs — sanity assessment

18 of 18 read · 8 surprising

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

## docs/hexload_log_writer_reference.rs

### the file itself — QUIRKY
- spec 3 · read at `cfc24e5692f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:49:32Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A reference/spec file (living in docs/, likely not compiled into the main crate) documenting or replicating the "hexload" tool's conversion-log writer — it collects conversion metadata (input files, detected backend/format, auxiliary files like cue sheets/artwork, errors, quality/compression settings) and formats it into a human-readable log file. Helper functions build up the report piece by piece (file size formatting, priority/format detection, error collection, quality settings) before write_conversion_log/generate_log_content assemble and write it out; the two test_* functions cover formatting edge cases like size units and compression ratio math.
- found: A full async conversion-log writer for the hexload-tui app: write_conversion_log collects a ConversionLogData struct (settings, input summary, per-file results, auxiliary files, errors) then generate_log_content renders it into a large human-readable text report (header, settings, input files, per-track results with merge-aware display, summary stats, errors, auxiliary files, footer). Extensive helpers derive the actual backend from executed commands, detect merged multi-track output, detect copy-mode (FLAC passthrough), and parse quality settings out of two different JSON option schemas (ConversionSettings vs ConversionOptions).
- predicted: some · documented: none · derivable: yes · legible: not judged · trap: no
- note: This lives under docs/ rather than src/ yet is a complete, non-trivial implementation (not just a spec) — worth confirming whether it's dead/reference code or actually wired into the build.

### `write_conversion_log`
- spec 3 · read at `b06c5f43c2c8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:16:53Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Gathers log data via collect_log_data (from results, config, conversion_options), builds the log text with generate_log_content, writes it to a file (e.g. conversion_log.txt) inside output_dir, and returns the resulting PathBuf wrapped in FeatureResult.
- found: Collects log data async, builds a timestamped filename "conversion-log-<ts>.txt", generates content, writes it via fs::write mapping IO errors to FeatureError::Permission, logs the path, and returns it.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `derive_actual_backend`
- spec 3 · read at `1a52390c4769` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:12:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates the ConversionResult list looking at the actual pipeline command(s) used (e.g. checking the command/program name against known backend binaries like ffmpeg or sox), and returns the detected backend name as a String; if no results or none match a known backend, falls back to returning the preferred string as-is.
- found: Collects distinct pipeline command program names (skipping known no-ops/utilities), maps known tool binaries to display names (FFmpeg, SoX, SSRC, FLAC encoder, WavPack, loudgain), joins multiple with " + ", falling back to preferred if nothing recognized.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: This is under docs/ as a reference copy, not the live source — worth noting for whoever indexes it.

### `collect_log_data` — QUIRKY
- spec 3 · read at `3c1f0ba4fc39` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:14Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates calls to sibling helper functions (derive_actual_backend, analyze_input_files, detect_auxiliary_files, detect_output_format, format_copy_options, collect_errors, detect_merged_output, format_from_conversion_settings, is_copy_mode, format_quality_settings_from_json, etc.) to gather each piece of data needed for a conversion log, then assembles and returns a populated ConversionLogData struct wrapped in FeatureResult, propagating errors from any of these sub-steps with ?.
- found: Generates a timestamp/session ID, calls several helper functions to analyze inputs, detect auxiliary files and output format, extracts quality settings and merge_to_single flag by parsing conversion_options JSON directly inline, computes merged_track_count and a source_type heuristic based on file count, then assembles ConversionLogSettings and ConversionLogData structs.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `generate_log_content` — TANGLED
- spec 3 · read at `e22691668071` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:10:55Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Central orchestrator that assembles the full formatted log text by calling most of its sibling helpers in sequence: detecting output/copy mode, formatting quality settings, file sizes, input file analysis, auxiliary files, priority/backend info, and any collected errors, concatenating everything (headers, sections) into one big output String representing the complete conversion log file content.
- found: Builds the full log text by pushing formatted sections in order: header, settings, input files summary, per-result conversion output (with a special merged-output branch using detect_merged_output, vs a normal per-file loop showing source info/commands/ReplayGain), a summary block computing success/failure counts, sizes, compression, and duration (with merge-aware totals computed inline), an errors section, an auxiliary-files section, and a footer.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no
- note: Most of the arithmetic and per-file formatting logic (merge detection branching, compression/duration math, source-info string building) is inlined directly in this function rather than delegated to the many similarly-named sibling helpers, despite their presence in the file.

### `analyze_input_files` — QUIRKY
- spec 3 · read at `e79f2001c6b6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:58Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the ConversionResults, for each one asynchronously reads file metadata (size) of the input path, and aggregates totals like file count and total byte size into an InputSummary, possibly also tracking distinct input formats. Returns FeatureResult so it can propagate an error if metadata reading fails for some file.
- found: Tallies input file extensions (uppercased) into counts, sorted descending, takes the parent dir of the first result's source_file as source_directory (defaulting to "Current directory"), and sums result.source_size for total_input_size — no actual async filesystem I/O despite the async signature; everything comes from fields already present on ConversionResult.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Function is declared async but performs no .await — likely async only to match a trait/call-site signature convention elsewhere in the (reference/example) file.

### `detect_auxiliary_files` — OBSCURE
- spec 3 · read at `9c0827a06542` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:55:56Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Since the parameter is named _output_dir (unused), this is likely a stub/placeholder that doesn't actually scan the filesystem — it probably just returns Ok(Vec::new()) or a hardcoded empty/sample list of AuxiliaryFile entries, standing in for a not-yet-implemented feature (e.g. detecting sidecar subtitle/chapter files alongside a converted output).
- found: Actually reads the output directory async, classifies each file by extension into auxiliary (txt/ini/jpg/jpeg/png/pdf/log) vs audio (opus/mp3/flac/aac/wav/aiff/cue), and collects name/size/action=\"Preserved\" for each auxiliary file found.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The leading underscore on _output_dir falsely signals an unused/stub parameter — it's fully used; misleading naming convention here.

### `format_priority`
- spec 3 · read at `76e528df3235` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:55Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Maps an i8 process/thread priority value (likely a nice-value-style range) to a human-readable label such as "Low", "Normal", "High", or similar tiers based on threshold comparisons, for display in a conversion log.
- found: Maps a nice-value-style i8 priority into buckets: -20..=-1 "High", 0 "Normal", 1..=10 "Low", else "Very Low".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `detect_output_format` — OBSCURE
- spec 3 · read at `b4d87d097352` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:00:33Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Since the parameter is prefixed with an underscore (unused), this likely just returns a hardcoded/placeholder format string like "Unknown" or similar constant, rather than actually inspecting the results — possibly a stub left for future implementation.
- found: Scans results for the first successful conversion, maps its output file extension to a display name (Opus/MP3/FLAC/AAC/WAV/AIFF or "Unknown (ext)"), falling back to "Unknown" if none found. The leading underscore on the parameter name was misleading — it's fully used.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Underscore-prefixed parameter names in this codebase don't reliably signal 'unused' — this one is used throughout the loop.

### `format_copy_options` — QUIRKY
- spec 3 · read at `5c3bcd75f0de` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:06:05Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Parses conversion_options as a JSON string, extracts copy-mode-specific settings (e.g. verify/preserve-timestamps flags), and formats them into a human-readable string for the log; returns an empty or default string when conversion_options is None or has no relevant fields. The _config parameter is unused.
- found: Parses conversion_options JSON, reads copy_auxiliary_files/copy_subdirectories booleans (defaulting true), and formats a differently-worded string depending on whether is_copy_mode is true (FLAC-copy phrasing with yes/no) vs normal conversion (enabled/disabled phrasing); falls back to a fixed default string on missing/unparseable input.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `collect_errors`
- spec 3 · read at `61a308855d5d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:38:14Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the ConversionResult slice, filters to results that contain an error/failure, and collects their error message strings into a Vec<String> for inclusion in the conversion log.
- found: Uses filter_map to clone each result's optional error_message field, collecting only the Some values into a Vec<String>.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: This file lives under docs/ (named "hexload_log_writer_reference.rs") rather than src/ — likely a generated reference copy, not the actual compiled source.

### `detect_merged_output`
- spec 3 · read at `d83640a00c68` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:32:23Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Groups results by output_file (e.g. via a HashMap from path to list of indices), then returns the first (or only) output_file/indices pair where more than one result shares that output_file, indicating a merge; returns None if no output_file is shared by multiple results.
- found: Builds a HashMap from output_file to the indices of results producing it, then returns the first entry with more than one index (a merge), or None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `format_file_size`
- spec 3 · read at `80e5fc6847fc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:58Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Converts a byte count into a human-readable string, choosing the appropriate unit (B, KB, MB, GB, ...) by repeatedly dividing by 1024, and formats the resulting value with a couple decimal places (e.g. "12.34 MB"), with bytes below 1024 shown as a plain integer with "B".
- found: Loops dividing by 1024 across B/KB/MB/GB, formats with 1 decimal place except plain integer bytes for the B case.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `format_from_conversion_settings` — OBSCURE
- spec 3 · read at `59cab9cb7416` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:12:16Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Extracts the "format" field from the ConversionSettings JSON value (e.g. via value.get("format").and_then(Value::as_str)), normalizes/uppercases it, and returns it as Some(String), or None if the field is missing or not a string. Likely doesn't include quality info itself since that's handled by the sibling format_quality_settings_from_json.
- found: Builds a human-readable, comma-joined summary line describing the conversion settings: handles copy mode specially, maps codec+quality descriptor to a display string per format (Opus bitrate table, FLAC compression level, MP3 bitrate, AAC profile, WAV/AIFF bit depth+rate), then conditionally appends dither, filter/nyquist, opus content type, verify, ReplayGain, and resample-quality parts based on whether the relevant settings apply (e.g. only when bit depth reduction or resampling is actually requested).
- predicted: none · documented: none · derivable: yes · legible: most · trap: no

### `is_copy_mode`
- spec 3 · read at `8fbdc561eab1` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:20:02Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Inspects a serde_json::Value (likely a log-data object) for fields indicating input and output format are both FLAC and that no transcoding/processing settings were applied (e.g. no quality/compression settings, or a "backend"/"mode" field equal to "copy"), returning true if this was a straight copy rather than a conversion.
- found: Handles two different JSON shapes (ConversionSettings with "format"/"bit_depth"/"sample_rate", and ConversionOptions with "output_format"/"target_sample_rate"/"target_bit_depth"/"dither_type"), and for each checks format=="Flac", no reencode_flac, no resample/bit-depth-change target, plus (in the second shape) an extra guard excluding dither with a 16/24-bit target from counting as copy mode.
- predicted: most · documented: some · derivable: no · legible: most · trap: no
- note: The two-schema handling and the specific dither+bit-depth exclusion in the second branch weren't derivable from the name/doc alone.

### `format_quality_settings_from_json` — QUIRKY
- spec 3 · read at `c70685e2937c` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:45Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Parses json_str as JSON into a generic Value (or tries ConversionSettings/ConversionOptions structs), pulls out quality-related fields (e.g. codec, bitrate, CRF/quality level, sample rate), and formats them into a human-readable summary string for the conversion log. Returns None if parsing fails or no recognizable quality fields are present.
- found: Parses JSON, distinguishes ConversionSettings (has 'format' field, delegates elsewhere) vs ConversionOptions; handles a FLAC-copy-mode shortcut; then for each output_format (Flac/Wav/Aiff/Mp3/Aac/Opus/WavPack) extracts and formats format-specific quality fields (compression level, bit depth/sample rate, CBR/VBR/ABR bitrate mode, etc), then appends ReplayGain, SSRC insane-mode, resample-quality-name, and nyquist filter info as suffixes. Returns None via ? on any missing/malformed field.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `test_file_size_formatting`
- spec 3 · read at `b24295e94143` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:43:19Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that calls format_file_size with several byte counts (e.g. 0, 512, 1024, 1_048_576, 1_073_741_824) and asserts the returned strings match expected human-readable sizes like "512 B", "1.0 KB", "1.0 MB", "1.0 GB".
- found: Tests format_file_size at three sizes (bytes, KB, MB) asserting human-readable strings like "500 B", "1.5 KB", "2.0 MB".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_compression_ratio`
- spec 3 · read at `065cfbfc75c9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:37:28Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Unit test that checks a compression-ratio calculation/formatting helper — likely feeding known input/output file sizes and asserting the resulting ratio string (e.g., a percentage or "1.5x" style reduction) matches expected values, similar in style to the neighboring test_file_size_formatting test.
- found: Constructs a ConversionResult with 10MB source_size and 8MB output_size, then asserts result.compression_ratio() returns 80.0 (output/source as a percentage), not a formatted string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: File lives under docs/ despite being a .rs test file — worth confirming whether it's a live test or a documentation snapshot/reference copy of real source.
