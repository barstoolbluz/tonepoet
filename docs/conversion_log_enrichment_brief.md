# Code task: Enrich conversion log with comprehensive per-track and pipeline details

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`

## Context

Read these files for background:
- `CLAUDE.md` — project overview, workspace structure
- `src/convert/pipeline/stages.rs` — find `build_conversion_log` (the function to enrich), `run_features` (the caller), and `write_durable_log` (the JSON log writer)
- `src/convert/pipeline/types.rs` — all pipeline data types: `TrackRecord`, `CommandRecord`, `AlbumOutcome`, `ArtifactSet`, `PipelineReport`, etc.
- `src/convert/pipeline/tool.rs` — `CommandRecord`, `ToolBinary`, `ProcessExit`
- `docs/hexload_log_writer_reference.rs` — vendored reference log writer from a predecessor project. **IMPORTANT: this code was NOT created by a reasoning model and may not be as rigorous, robust, correct, performant, or idempotent as it should be. Use it as inspiration for structure and field coverage, NOT as code to copy verbatim. The tonepoet pipeline has different (richer) data structures that should be used directly.**

## What already exists

**`build_conversion_log()`** in `stages.rs` produces a minimal plain text log:
- Timestamp
- Source container path
- Target format
- Track count
- Result (Complete/Partial/Blocked)

That's all. No per-track details, no conversion settings, no timing, no sizes, no commands.

**`write_durable_log()`** writes the full `PipelineReport` as JSON — machine-readable, contains everything, but not human-readable.

**The conversion log file** (`conversion_log.txt`) is written by `run_features()` as a sidecar alongside the audio files. It goes into the album output directory.

## What data is available

All of this is already in the pipeline structs at the time `build_conversion_log` is called:

### From `AlbumOutcome` (passed as `outcome`):

**Per-track records** (`Vec<TrackRecord>`):
- `track_id`: source ordinal, track number, disc number
- `outcome`: Ok or Err(String) with error text
- `source_ref`: how the track was obtained (staged file, image segment, SACD track)
- `realized_input`: path to the decoded input file
- `output_file`: path to the encoded output file (staged)
- `commands: Vec<CommandRecord>` — every tool invocation for this track
- `bytes_in: Option<u64>` — source file size
- `bytes_out: Option<u64>` — output file size
- `duration: Option<Duration>` — encode elapsed time

**Per-command records** (`CommandRecord` in `tool.rs`):
- `binary: ToolBinary` — which tool (Ffmpeg, Sox, Metaflac, Loudgain, etc.)
- `sanitized_args: Vec<String>` — full command line with secrets redacted
- `exit: Option<ProcessExit>` — exit code or signal
- `stdout_tail: String` — last 64 KiB of stdout
- `stderr_tail: String` — last 64 KiB of stderr
- `elapsed: Duration` — per-command execution time

**Stage records** (`Vec<StageRecord>`):
- `stage: PipelineStage` — which stage (Materialize, Convert, Merge, Metadata, ReplayGain, etc.)
- `outcome: StageOutcome` — Ok, Skipped, or Failed(String)

### From `PreparedSource` (passed as `source`):

**Album metadata**:
- album, album_artist, genre, date, total_tracks, total_discs, disc_number
- `extra: BTreeMap<String, String>` — all additional metadata (catalog number, MusicBrainz IDs, country, etc.)

**Per-track metadata** (`PreparedTrack.metadata`):
- title, artist, album_artist, composer, performer, genre, date
- track_number, disc_number, isrc
- `extra: BTreeMap<String, String>` — per-track custom fields

**Per-track technical info**:
- `sample_rate: u32`
- `bit_depth: Option<u32>`
- `expected_samples: Option<u64>`

### From `PipelineRequest` (passed as `req`):

- `target_format: AudioFormat` — output format
- `encode: EncodeOptions` — backend, bitrate, compression_level, dither policy
- `merge: bool` — whether tracks were merged
- `naming: NamingPolicy` — filename and folder templates
- `stages: StagePolicy` — which stages were enabled/disabled

## What this task delivers

Rewrite `build_conversion_log()` to produce a comprehensive, human-readable plain text conversion log. The function signature can be expanded if needed (e.g., to accept `&ArtifactSet`), but check `run_features()` for what's available at the call site.

### Required sections in the log

**1. Header**
- Title: "TONEPOET CONVERSION LOG"
- Generated timestamp (UTC)
- Job ID and Item ID (from `req`)

**2. Source Information**
- Container path (req.container)
- Source kind (source.kind — SevenZip, CueImage, SacdIso)
- Track count
- Album metadata: artist, album, year, genre, catalog number (from extra)

**3. Conversion Settings**
- Target format (req.target_format.name())
- Encode backend (req.encode.backend)
- Bitrate (if set)
- Compression level (if set)
- Dither policy
- Merge mode (req.merge)
- Stages enabled/disabled: display as "Metadata: Enabled", "ReplayGain: Disabled", etc. from req.stages.metadata/replaygain/features (StageRequirement enum: Enabled or Disabled)
- Naming templates (folder + filename)

**4. Per-Track Results**
For each track in the outcome (whether successful or failed):
- Track number and title (from source.tracks matching by source_ordinal — both TrackRecord.track_id and PreparedTrack.id contain a 1-based source_ordinal field; if title is None, show "Track N (untitled)")
- Status: success or failure (with error message)
- Source info: sample rate, bit depth, expected samples (omit if no matching PreparedTrack found)
- File sizes: bytes_in → bytes_out with compression ratio
- Encode duration
- Commands executed: tool binary + sanitized args + elapsed time + exit status. Format exit as: "exit 0" for success, "exit 1 (error)" for nonzero, "killed by signal 9" for signal termination, "exit unknown" if missing. ProcessExit is an enum with variants Code(i32), Signal(i32), Unknown — see tool.rs.
- If failed: error message from TrackOutcome::Err

Optional metadata fields (title, artist, composer) may be None. Omit the field rather than showing "None". For title, always show at least "Track N" as a fallback.

**5. Stage Summary**
For each stage that ran:
- Stage name
- Outcome (Ok, Skipped, or Failed with error)

**6. Overall Summary**
- Total tracks: successful / failed / total
- Total input size and output size with overall compression ratio
- Total conversion time (sum of track durations)
- Result: Complete, Partial (N/M ok), or Blocked (reason)

**7. Footer**
- "Log generated by tonepoet"

### Formatting guidelines

- Plain text, UTF-8
- Use clear section headers with separators (e.g., `===` or `---`)
- Align columns where practical (file sizes, durations)
- Human-readable file sizes (B, KB, MB, GB)
- Human-readable durations (Xm Ys or Xs)
- Redact nothing extra beyond what CommandRecord.sanitized_args already handles
- Do NOT include stdout_tail or stderr_tail in the log (too verbose; they're in the JSON durable log)

### Reference

See `docs/hexload_log_writer_reference.rs` for the structure and formatting used by a predecessor project's log writer. **IMPORTANT**: that code was created outside of a reasoning model context and may not be rigorous, correct, or idiomatic. Use it only as inspiration for WHAT to include, not HOW to implement it. The tonepoet pipeline has richer data structures (CommandRecord with per-command timing, TrackRecord with bytes_in/bytes_out, etc.) that should be used directly.

## Locked contracts (do not change)

- `run_features()` function signature — if `build_conversion_log` needs more data, update the call site within `run_features` to pass it, but do not change `run_features`' own signature
- `AlbumOutcome`, `TrackRecord`, `CommandRecord` structs — read-only, do not modify
- `PipelineReport`, `write_durable_log` — the JSON log path is separate, do not modify
- `SidecarArtifact`, `SidecarKind` — the log is still a `SidecarKind::ConversionLog` sidecar

## Files modified

| File | Changes |
|------|---------|
| `src/convert/pipeline/stages.rs` | Rewrite `build_conversion_log()` to produce comprehensive output; update call site in `run_features()` if additional parameters needed |

## Helper functions

You will likely need small helpers for:
- `format_bytes(bytes: u64) -> String` — human-readable file sizes
- `format_duration(d: Duration) -> String` — human-readable durations
- `compression_ratio(bytes_in: u64, bytes_out: u64) -> String` — percentage reduction/increase

These should be private functions in `stages.rs` near `build_conversion_log`.

## Tests required

- `build_conversion_log` with a Complete outcome produces all sections
- `build_conversion_log` with a Partial outcome shows both successful and failed tracks
- `build_conversion_log` with a Blocked outcome shows the block reason
- Per-track details include sizes, duration, and command info
- Stage summary includes all stages that ran
- Helper functions: format_bytes, format_duration, compression_ratio produce expected output

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`.

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Production-ready changes to `stages.rs`. Must compile and pass `cargo test --lib`. The pipeline tests (`cargo test --lib convert::pipeline`) currently pass 223 tests and must not regress. Some unrelated TUI tests may fail due to environmental factors — focus on pipeline test stability.
