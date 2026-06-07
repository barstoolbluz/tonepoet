# Implementation brief: remaining DSD/DST integration work

## Current state

sacd-rs is a 19K-line pure-Rust SACD/DSD library (GPL-2.0-or-later) with:
- SACD ISO sector-level extraction with integrity reporting and salvage mode
- DST frame decoder: stateful, supports 1-6 channels, DSD64/128/256, full MPEG-4 DST syntax
- DST frame encoder: verified predictive encoding for stereo and 6-channel, configurable effort, raw fallback policy
- DFF/DST container writer (`DffDstWriter`) with DSTF/DSTC/DSTI/FRTE chunks
- DSF, DFF/DSD, DFF/DST file readers (`DsfStreamReader`, `DsdDsdiffStreamReader`, `DstDsdiffStreamReader`)
- Unified DSD source model (`open_dsd_source`, `DsdSource` trait) bridging SACD ISO, DSF, and DSDIFF
- DSD file validation (`validate_dsd_stream`), corpus validation, container inspection
- Transactional output materialization (`OutputTransaction`)
- 252 library tests passing

Tonepoet integration:
- SACD ISO materializer (`materializer_sacd.rs`) parses TOC into `PreparedTrack` metadata
- Realization stage (`stages.rs`) extracts to DSF or DFF via `extract_track`
- Plan bridge (`plan_bridge.rs`) detects realized format from extension, feeds planner
- Format pills include DSF and DFF; planner handles both via `is_dsd()`
- Pipeline does NOT yet accept standalone DSF/DFF files as input sources

Two external Rust crates are included as **reference material only** (not dependencies):
- `bleggett/dst-decoder` (Apache-2.0) — SIMD-optimized DST decoder with pre-allocated buffers
- `KyokoMiki/cladst` (GPL-3.0) — DST encoder/decoder + DSF/DFF container I/O

## Implementation goals

### 1. Differential test oracle

Add differential validation of sacd-rs DSF/DSDIFF/DST reading, DST decoding, and DFF/DST writing.

**What to compare** (for each oracle source):
- Decoded DSD bytes (byte-exact match)
- Frame counts (total emitted)
- CRC status per frame (passed/failed/missing)
- Channel count and sample rate from container headers
- Duration / total sample count
- DSDIFF chunk layout (chunk IDs, offsets, sizes)
- DSTF payload offsets and sizes
- First failing frame index and error message

**Oracle sources** (in priority order):
1. **Self-referential round-trip**: encode DSD→DST→decode, verify byte-exact match with original DSD
2. **Fixture vectors**: the 13 existing stereo + 6ch DST/DSD fixture pairs in `crates/sacd-rs/src/dst/fixtures/`
3. **Generated oracle vectors**: synthetic DSD patterns (silence=0x69, alternating, ramp) encoded and decoded
4. **FFmpeg/ffprobe**: optional external-tool gate — probe container metadata of generated DSF/DFF files
5. **sacd_extract**: optional external-tool gate — compare extraction output byte-for-byte

**Test structure**:
- Normal unit tests (`#[test]`): must not require external binaries. Use self-referential round-trips and fixture vectors.
- External-tool tests (`#[test] #[ignore]`): gated on tool availability. Compare against ffprobe metadata, sacd_extract output.
- Corpus harness: `scripts/validate_dsd_corpus.sh` or equivalent for batch validation against real-world discs.

**Files to modify**: `crates/sacd-rs/src/dst/mod.rs` (round-trip tests), `crates/sacd-rs/src/dsd_file/ops.rs` (validation tests), `crates/sacd-rs/src/extract.rs` (extraction oracle tests), new test module or file as needed.

### 2. Caller-supplied DST decode buffer

Add a non-allocating decode API that accepts a caller-supplied output buffer.

**New public API** (in `crates/sacd-rs/src/dst/decoder.rs` or `dst/mod.rs`):
```rust
pub fn decode_frame_into(input: &[u8], channel_count: u8, output: &mut [u8]) -> Result<usize, DstError>;
pub fn decode_frame_with_rate_into(input: &[u8], channel_count: u8, rate: DstRate, output: &mut [u8]) -> Result<usize, DstError>;
```

**Requirements**:
- Preserve existing `decode_frame` / `decode_frame_with_rate` as convenience wrappers that allocate internally
- Validate output buffer size before decoding: return `DstError` variant for undersized buffers
- Return exact bytes written on success
- Structured errors for: undersized buffer, oversized decoded output, malformed DST, unsupported rate/channel

**Streaming reader integration**: Update `DstToDsdAdapter` (or equivalent in `dsd_file/reader.rs` / `dsd_file/source.rs`) to reuse a single buffer across frames instead of allocating a fresh `Vec<u8>` per DST frame decode.

**Tests**: Round-trip correctness with caller-supplied buffer. Undersized buffer rejection. Buffer reuse in adapter (verify no per-frame allocation via frame count vs allocation pattern).

**Files to modify**: `crates/sacd-rs/src/dst/decoder.rs`, `crates/sacd-rs/src/dst/mod.rs`, `crates/sacd-rs/src/dsd_file/reader.rs`, `crates/sacd-rs/src/dsd_file/source.rs`

### 3. Clean-room DST encoder table compression (research path)

**Provenance constraint**: Do not copy cladst code, do not vendor cladst, do not add cladst as a dependency. This is a clean-room implementation inspired only at the architectural level by public DST specifications (MPEG-4 Part 3 Subpart 10).

**Candidate improvement areas** (for future implementation):
- Rice-coded table representation: evaluate whether the current coefficient serialization can be made more compact via better Rice parameter selection
- Compact FIR/probability table emission: minimize header overhead per frame
- Table selection heuristics: adaptive prediction order based on signal characteristics
- Rate-distortion or size-based frame candidate selection: try multiple prediction orders, pick the smallest
- Post-encode verification: decode every encoded frame and verify byte-exact match (already partially implemented)

**Deliverable for this pass**: A documented implementation plan in the brief, not code. The plan should specify which encoder APIs would change, what new internal types are needed, and what tests would validate improvement. Include licensing notes: implementation is safe under GPL-2.0+ only if done clean-room from the MPEG-4 spec; if cladst patterns are recognizable, GPL-3.0+ licensing decision is required.


**Research-plan deliverable**:

- **Encoder API changes**:
  - Extend `DstEncoderOptions` with a disabled-by-default `table_compression` field, using a public enum such as `DstTableCompressionMode::{Disabled, SpecRice, Search}`. `Disabled` preserves the current uncoded-table bitstream, so existing callers get byte-for-byte comparable behavior unless they opt in.
  - Add optional search controls to the same options struct, for example `rice_parameter_range`, `maximum_table_header_bits`, and `maximum_table_search_candidates`. These settings should affect only predictive DST generation, not source-DST passthrough, caller-supplied `write_dst_frame`, or explicit raw fallback.
  - Extend `DstSelectedPredictor` and `DstFrameEncodeTelemetry` with table-coding metadata: selected FIR coding mode, selected probability-table coding mode, Rice parameters tried, selected Rice parameters, per-table header-bit cost, per-table payload-bit cost, and accepted candidate size. This gives the app and corpus tools enough data to compare uncoded versus Rice-coded tables without parsing bitstreams after the fact.
  - Keep `encode_frame_interleaved` and `encode_frame_interleaved_with_telemetry` as the public entry points. Do not add a separate encoder path unless profiling shows the search state cannot stay internal without hurting the current API.

- **Internal types**:
  - `TableCodingMode`: internal enum for `Uncoded`, `Rice { parameter: u8 }`, and any spec-defined variants confirmed from MPEG-4 Part 3 Subpart 10.
  - `RiceParameterCandidate`: selected parameter plus estimated signed-symbol payload cost, table-length overhead, escape/overflow count, and total bit cost.
  - `RiceParameterSelector`: deterministic selector that evaluates the configured parameter range and returns the lowest-cost spec-valid candidate for a coefficient or probability table.
  - `EncodedTablePlan`: per-table plan containing the original values, normalized signed/unsigned representation, coding mode, bit count, and writer callback/input for `BitWriter`.
  - `FrameTablePlan`: the set of FIR and probability `EncodedTablePlan`s for one predictive frame, linked to the existing `DstTableStrategy` and `DstSelectedPredictor` metadata.
  - `TableCodingAudit`: test-only or debug-only record of every rejected table-coding candidate and the reason it lost, so corpus runs can explain regressions.

- **Validation tests**:
  - Unit-test Rice parameter selection on hand-authored coefficient/probability arrays, including all-zero tails, alternating signs, maximum legal coefficient magnitudes, one-symbol tables, and 128-entry tables.
  - Round-trip encode/decode tests for stereo and six-channel DSD64 fixtures with `table_compression = Disabled` and `table_compression = SpecRice`; decoded DSD must match the source byte-for-byte.
  - Candidate-selection tests showing the encoder keeps the uncoded table when Rice coding is larger and selects Rice coding only when the total frame size falls.
  - Bitstream-structure tests that parse the produced frame header and verify table-coding flags, Rice parameters, table lengths, and arithmetic payload offsets.
  - Corpus tests reporting per-frame table-coding mode, candidate count, selected bit cost, final frame size, and verification outcome. These should be normal unit tests for synthetic/fixture vectors and `#[ignore]` tests for external fixtures or ffmpeg/sacd_extract gates.
  - Regression tests proving source-DST passthrough and caller-supplied `write_dst_frame` never decode-and-reencode or rewrite table coding.

- **Licensing/provenance guardrails**:
  - Implement only from MPEG-4 Part 3 Subpart 10 plus in-tree decoder behavior. The `reference_only/cladst` files may inform terminology at an architectural level, but no code, constants, table layouts, tests, or control-flow patterns should be copied.
  - Put the spec sections and page/paragraph references used for each table-coding rule in code comments or a short design note before implementation begins.
  - Keep the first implementation behind an opt-in `DstEncoderOptions` mode until fixtures and corpus runs prove decode verification, size improvement, and external-tool acceptance.
  - If review finds recognizable cladst-derived structure in the implementation, stop treating it as GPL-2.0-or-later work and make an explicit GPL-3.0-or-later licensing decision before merging.

**Files to inspect**: `crates/sacd-rs/src/dst/encoder.rs` (current encoder), `reference_only/cladst/src/codec/rice.rs` and `reference_only/cladst/src/codec/frame.rs` (architectural reference only)

### 4. Encoder rate/channel policy expansion

Separate these orthogonal policies explicitly in the encoder/writer API:

| Policy | Scope | Current support |
|--------|-------|----------------|
| DFF/DST container support | 1-6 channels, any DSD rate | Supported |
| Source DST passthrough | 1-6 channels, any rate | Supported via `write_dst_frame` |
| Caller-supplied encoded DST | 1-6 channels, any rate | Supported via `write_dst_frame` |
| Raw DST fallback (DSTCoded=0) | 1-6 channels, any rate | Explicit opt-in only |
| Predictive DST generation | 2 and 6 channels, DSD64 only | Verified correct |

**Goal**: Make predictive generation rate-aware (DSD64/128/256) where the codebase supports it safely. The encoder's FIR/AC/Rice logic is rate-independent in principle — the frame geometry changes (4704 vs 9408 vs 18816 bytes per channel) but the algorithm is the same. Extend `DstRate` awareness through the encoder if the decoder already handles it.

**Requirements**:
- Tests for DSD64/128/256 geometry, even if DSD128/256 predictive generation is expected to reject unsupported configurations cleanly
- No silent raw fallback for unsupported rates
- No silent DST re-encoding of source DST
- Preserve existing policy: predictive only where verified, container/passthrough for all legal layouts

**Files to modify**: `crates/sacd-rs/src/dst/encoder.rs`, `crates/sacd-rs/src/dff_dst_writer.rs` (policy enforcement), `crates/sacd-rs/src/extract.rs` (rate-aware extraction)

### 5. App-level DSF/DFF input integration

**This is the largest product-level gap.** The library has `open_dsd_source()`, DSF/DSDIFF readers, and validation helpers. The pipeline needs to accept standalone DSF and DFF files as conversion inputs — not just SACD ISOs.

**Integration plan**:

1. **Format detection** (`src/convert/formats.rs`): `FormatDetector::detect` already recognizes `.dsf` and `.dff` extensions and returns `AudioFormat::Dsf` / `AudioFormat::Dff`. Verify this works for the CLI path.

2. **Source routing** (`src/convert/processor.rs`): DSF/DFF files currently take the legacy single-file conversion path (ffmpeg decode). For DSD→DSD conversion (e.g., DSF→DFF), this path works through the planner's existing `plan_from_dsd` / `plan_to_dsd`. For DSD→PCM, the planner routes through sox. No materializer is needed — standalone DSD files are `TrackSourceRef::StagedFile`.

3. **Plan bridge** (`src/convert/pipeline/plan_bridge.rs`): `planner_format_from_path` already detects `.dsf` → `PlannerFormat::Dsf` and `.dff` → `PlannerFormat::Dff`. The planner sees these as DSD sources.

4. **Planner source metadata**: The `SourceInfo` struct in the planner needs to carry:
   - Source kind (SacdIsoTrack, Dsf, DsdiffDsd, DsdiffDst)
   - Sample rate
   - Channel count
   - Total samples (or unknown)
   - Whether DST-compressed (for DFF/DST inputs)
   - Validation status

   Currently `plan_bridge.rs:source_info_for_realized_track` probes the realized file. For DSF/DFF inputs on the legacy single-file path, the probe comes from `tui/probe.rs` (ffmpeg-next). Verify that ffmpeg-next correctly probes DSF and DFF files for sample rate, channel count, and codec.

5. **Distinguish DFF/DSD from DFF/DST**: Both use `.dff` extension. The plan bridge should inspect the file (check CMPR chunk for "DSD " vs "DST ") to set the correct source kind. Use `inspect_dsd_container` from `dsd_file/inspect.rs` for this.

6. **Preserve SACD ISO materializer**: No changes to `materializer_sacd.rs`. SACD ISOs continue to route through the materializer → realization → planner path.

**Tests**:
- DSF input → FLAC output (DSD→PCM conversion)
- DFF/DSD input → DSF output (DSD→DSD container conversion)
- DFF/DST input → DSF output (DST decode → DSF write)
- SACD ISO input → DSF output (unchanged behavior)
- DFF/DST input correctly identified as DST-compressed (not plain DSD)

**Files to modify**: `src/convert/pipeline/plan_bridge.rs` (DFF/DST detection), `src/convert/formats.rs` (verify detection), possibly `src/tui/probe.rs` (verify ffmpeg-next DSD probing)

### 6. Progress/stat reporting

Add library-level and app-level progress/stat reporting for DSD/DST operations.

**Library-level stats** (already partially present in `ExtractStats`, `ExtractIntegrityReport`, `DffDstWriterStats`):
- bytes read / bytes written
- frames read / frames decoded / frames emitted
- CRC checked / passed / failed / missing
- DST passthrough frames / decoded frames / reencoded frames / raw fallback frames
- First error offset / frame index
- Elapsed time (if project conventions allow)

**App-level integration**: The conversion log (recently redesigned) should surface DSD/DST-specific stats when a conversion involves DSD sources. The `CommandRecord.description` field already carries planner step descriptions. Add DSD-specific detail to the log's conversion summary when source is DSD.

**Reuse existing conventions**: `PipelineEvent` for progress, `StageRecord` for stage outcomes, `TrackRecord.commands` for per-track command records. Do not invent new reporting infrastructure.

**Files to modify**: `src/convert/pipeline/stages.rs` (conversion log DSD section), existing sacd-rs stat types (verify completeness)

## Explicit non-goals

- Do not integrate bleggett's unsafe/SIMD paths
- Do not copy bleggett wholesale
- Do not adopt bleggett's "write DSD silence on decode error" behavior
- Do not copy cladst source code or tests directly
- Do not add cladst as a dependency
- Do not adopt cladst's default decode-and-reencode behavior for existing DST input
- Do not replace tonepoet's SACD ISO parser/materializer
- Do not make DFF/DST the implicit meaning of DFF — distinguish by inspection
- Do not claim world-class or external interoperability unless tests prove it

## Provenance and licensing

- sacd-rs is GPL-2.0-or-later (compatible with GPL-3.0)
- bleggett/dst-decoder is Apache-2.0 (compatible, reference only)
- cladst is GPL-3.0 (compatible with tonepoet's GPL-3.0, but sacd-rs clean-room implementation avoids direct code derivation)
- DST encoder improvements must be clean-room from MPEG-4 Part 3 Subpart 10 spec
- Reference material in the bundle is marked `reference_only/` and must not be copied into tonepoet

## Expected failure modes

- DSD128/256 predictive DST generation may not compress well — tests should expect clean rejection, not silent fallback
- Mono channel predictive DST is unsupported — test for clean error
- DFF/DST files with non-standard DST segmentation may fail to decode — test should surface structured error
- ffmpeg-next may not probe DSD rates correctly for all DSD multiples — verify empirically

## Definition of done

1. `cargo test --lib --workspace` passes (currently 1345 tests)
2. DST decoder has `decode_frame_into` / `decode_frame_with_rate_into` non-allocating API
3. Streaming DST readers reuse decode buffers
4. Round-trip differential tests: encode→decode byte-exact for stereo and 6ch at DSD64
5. Encoder policy types explicitly separate container/passthrough/predictive/raw support
6. DSD128/256 encoder tests exist (pass or reject cleanly)
7. Standalone DSF/DFF files are accepted as conversion inputs through the CLI
8. DFF/DST inputs are correctly distinguished from DFF/DSD by inspection
9. Conversion log surfaces DSD/DST stats when applicable
10. No new external dependencies added

## Risks and manual-review points

- **Encoder round-trip at DSD128/256**: The FIR prediction constants and frame geometry scale, but no real-world DSD128/256 DST fixtures exist in our test suite. Manual validation against real discs is needed before claiming support.
- **ffmpeg-next DSD probing**: Whether ffmpeg-next correctly reports DSD sample rates and bit depths for DSF/DFF files needs empirical verification — it may report PCM-equivalent rates.
- **DFF/DST detection**: The CMPR chunk inspection must handle both big-endian DSDIFF and edge cases (missing PROP chunk, truncated files) gracefully.
- **Clean-room boundary**: The DST encoder table compression work must be reviewed for provenance if it resembles cladst's approach.

## Files to read (required context for implementation)

| File | Purpose |
|------|---------|
| `crates/sacd-rs/src/lib.rs` | Module map |
| `crates/sacd-rs/src/extract.rs` | Extraction orchestrator, OutputFormat, DST options |
| `crates/sacd-rs/src/dst/decoder.rs` | DST decoder — add decode_frame_into here |
| `crates/sacd-rs/src/dst/encoder.rs` | DST encoder — rate/channel policy expansion |
| `crates/sacd-rs/src/dst/mod.rs` | DST public API facade |
| `crates/sacd-rs/src/dst/tables.rs` | Constants and lookup tables |
| `crates/sacd-rs/src/dst/bitreader.rs` | Bit-level reader |
| `crates/sacd-rs/src/dsd_file/mod.rs` | DSD file facade |
| `crates/sacd-rs/src/dsd_file/reader.rs` | DSF/DFF/DST stream readers |
| `crates/sacd-rs/src/dsd_file/source.rs` | DsdSource trait, adapters |
| `crates/sacd-rs/src/dsd_file/ops.rs` | Validation, stream copy |
| `crates/sacd-rs/src/dsd_file/inspect.rs` | Container inspection |
| `crates/sacd-rs/src/dsd_file/asset.rs` | DSD asset model |
| `crates/sacd-rs/src/dsd_file/corpus.rs` | Corpus validation |
| `crates/sacd-rs/src/dsd_file/policy.rs` | Channel/format policies |
| `crates/sacd-rs/src/dff_dst_writer.rs` | DSDIFF/DST container writer |
| `crates/sacd-rs/src/dsf_writer.rs` | DSF writer |
| `crates/sacd-rs/src/dff_writer.rs` | DFF/DSD writer |
| `crates/sacd-rs/src/dff_footer.rs` | DSDIFF footer chunks |
| `crates/sacd-rs/src/id3.rs` | ID3v2.4 for DSF |
| `crates/sacd-rs/src/output_transaction.rs` | Transactional output |
| `crates/sacd-rs/src/iso_reader.rs` | ISO sector reader |
| `crates/sacd-rs/src/frame.rs` | SACD sector parser |
| `crates/sacd-rs/src/consts.rs` | Scarlet Book constants |
| `crates/sacd-rs/src/test_util.rs` | Test helpers |
| `crates/sacd-rs/src/source_model.rs` | Compat re-export |
| `crates/sacd-rs/src/stream_reader.rs` | Compat re-export |
| `crates/sacd-rs/src/stream_ops.rs` | Compat re-export |
| `crates/sacd-rs/src/container.rs` | Compat re-export |
| `crates/sacd-rs/src/corpus.rs` | Compat re-export |
| `crates/sacd-rs/src/asset_model.rs` | Compat re-export |
| `src/convert/pipeline/materializer_sacd.rs` | SACD materializer |
| `src/convert/pipeline/stages.rs` | Realization + validation |
| `src/convert/pipeline/types.rs` | TrackSourceRef, PreparedTrack |
| `src/convert/pipeline/plan_bridge.rs` | Format detection for planner |
| `src/convert/processor.rs` | Dispatch logic |
| `src/convert/formats.rs` | AudioFormat enum, detection |
| `tonepoet-pipeline/src/enums.rs` | Planner AudioFormat |
| `tonepoet-pipeline/src/plan.rs` | Conversion planner |
| `CLAUDE.md` | Project conventions |
