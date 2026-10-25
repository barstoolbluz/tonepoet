# IP-02 — FLAC ranged-source evidence and handoff

**Session date:** 2026-10-24 (America/New_York).  **Status:** `implemented-unqualified`; production S3 admission remains **OFF**.

## What changed

- Added the inert `src/convert/pipeline/cue_flac_range.rs` typed command adapter. It accepts a sample-indexed half-open range bound to a `CueStreamSegment`, checks the source against the same actual STREAMINFO prefix used by the CUE materializer, and constructs an exact `s32le` FFmpeg producer command with an independently checkable expected byte count. It cannot run an encoder, select S3, publish an output, or change the existing S1 transport.
- Exported a minimal STREAMINFO fact tuple from `materializer_cue.rs` rather than inventing a second FLAC metadata parser.
- Added Rust boundary/unit tests (requires build host), and a regenerable Python/native FFmpeg range qualification suite. The latter compares **all bytes**, not just durations or compressed checksums.

## Decoder choice and boundary proof

`ffmpeg -accurate_seek -ss <INTEGER_SECONDS> -err_detect crccheck+explode -i <PHYSICAL_FLAC> ... -af atrim=start_sample=<INTEGER_OFFSET>:end_sample=<INTEGER_OFFSET_PLUS_LENGTH>,asetpts=N/SR/TB -f s32le -c:a pcm_s32le pipe:1`

`-ss` is deliberately **before** input. The integer-second anchor seeks into FLAC rather than replaying the whole audio prefix as a filter-only implementation would. The `atrim` counter is relative to the first sample emitted after FFmpeg's accurate seek, and the anchor's integer-second position is exact at the declared source sample rate. Thus all trim positions stay integers, including 44,100/75=588 frame offsets and 587/589 offsets. For this packet the supported rate/depth/channel cells are only 44,100/48,000/96,000 Hz × 16/24-bit integer × 1/2 channels; one physical FLAC file's STREAMINFO must agree with the supplied CUE facts. Anything outside these cells is refused.

`crccheck+explode` is **required**. An earlier exploratory version with only `explode` emitted an apparently full-length but sample-corrupted output from an intentionally CRC-damaged FLAC frame, returning status zero. The corrected command detects it and returns a nonzero status. This is an actual defect discovered by adversarial qualification, not a stylistic hardening request. Frame CRC is local; STREAMINFO's whole-stream MD5 does **not** certify independently selected ranges without an additional full-stream pass. No such pass was invented here.

Output extent is checked with `u128`, and the checked verifier requires exactly `(end-start) × channels × 4` bytes. A successful FFmpeg exit does not itself guarantee full extent; the next packet must *actually insert* this verifier into the supervised producer/consumer streaming boundary before launching any parallel workers. The current adapter is intentionally not a new `ToolRunner` implementation.

## Alternatives not selected

- `atrim` from source sample zero: sample exact but decodes O(start) input for every worker, failing the independent-source objective.
- Floating-point seconds for both seek and final trim: can round fractional samples and cannot serve as the sole CUE boundary authority.
- Native `flac --skip/--until`: an independent candidate for a future corrective packet if the qualified FFmpeg behavior fails; its native raw s16/s24 PCM needs explicit transformation to match the existing widened `s32le` carrier. Do not substitute it by changing planner input.
- One FFmpeg invocation with post-input `-ss`: not selected as the independent seek contract; post-input seeking may force decoding the prefix.

## Qualification boundary

`native_qualify_ranges.py` generates per-sample deterministic PCM signatures in 24 FLAC cells (all rate × depth × channel cases, with and without SEEKTABLE) and long/no-seektable and damaged-source cases. It decodes the **same source** through existing S1's raw PCM FFmpeg arguments to establish a canonical decoded reference. For every range it verifies the expected byte length and compares PCM bytes at the matching reference offset; disjoint tiles must hash to the whole-stream reference. The script also exercises slow stdout consumption, cancellation and explicit child reap, CRC damage, truncation, late-offset timing and independent reference widening. All generated large audio stays outside the source archive; SHA-256s are recorded in the JSON results.

Both the quick (24 cells, 552 ranges) and full (24 cells, 840 ranges) corrected native runs completed with zero mismatches. The full run tiled 264 intervals, exercised 75-second long-source seeking and checked corrupted/truncated input; its measured late/early median timing ratio was 1.077x on this host. Tool path hashes, audio/reconstruction SHA-256s, and exact commands are in `native_host_results_full.json`. The tests do not measure whole TonePoet album speedup.

A valid result is **tool-version-specific native decoder evidence only**. The following gates are still on the receiving Rust build host, against the exact delivered full-source archive:

```sh
sha256sum TonePoet_CUE_IP02_FULL_SOURCE_*.tar.gz
# Extract into a clean temporary directory, verify IP02_SOURCE_SHA256SUMS.
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_ip02 -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

**Do not count zero matching tests as a pass**. Capture exact exit codes and any known unrelated failures rather than hiding them. Run the native script on the build host with its actual FFmpeg version, using `--workdir` and `--output` paths outside the source tree; compare the resulting version hash/version cell to these results. An exact installed binary/version and executing platform must be qualified; portable command correctness alone is insufficient. If any field differs or Rust checks fail, leave S3 disabled and record a narrow corrective packet, not an unqualified acceptance.

The new Rust adapter is entirely unreferenced by ordinary conversion routes. The existing S1 path, failure policy, ReplayGain, planner, metadata, logging, cancellation, and publication are unchanged. ToolRunner's two-child `run_pipeline` remains Unix-only. These are independent hard gates for IP-03/IP-04 even after a successful seek experiment.

## Rollback and durable continuation

The immutable input is `TonePoet_SNAPSHOT_847fc5e_2026-10-24.tar.gz`, SHA-256 `880c5e2693ee09ef56723c0ac7b72cbd6080340c08671f0f36e9288b9a994112`. Removing the inert module declaration and helper restores the exact previous code behavior; no production route flags need changing. Current progress and failures are written after each native cell into its JSON checkpoint, and the final cumulative status/next step is in the planning authority's `NEXT_SESSION_HANDOFF.md` and this directory's `SESSION_CHECKPOINT.md`.
