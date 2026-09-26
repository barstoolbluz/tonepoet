# Delivery notes: Analyze ID3 wrapper repair and Analysis correctness audit

Date: 2026-09-26
Base: bundled `main` head `2cc64c2` (`v0.5.3`)
Supersedes: `tonepoet_analyze_id3_wrapper_repair_delivery_r1_2026-09-26.tar.gz`

## Outcome

This delivery keeps the completed ID3-wrapped FLAC Analyze/Repair work and fixes the seven confirmed Analysis defects from the expanded 2026-09-26 audit. The corrections stay local to Analysis and its report/cache/UI seams. No general job framework, cache dependency graph, file-rewrite redesign, or unrelated architecture refactor was introduced.

### ID3-wrapped FLAC Analyze/Repair

- `:analyze` reports an ID3v2 prefix, an ID3v1 trailer, or both on affected native FLAC files.
- The Analysis overlay exposes `r Repair` only when a displayed result is wrapped.
- Repair removes only the detected wrappers and reuses the existing streaming/atomic FLAC replacement path with its source revalidation, metadata preservation, hardlink/symlink refusal, write admission, and durability handling.
- Clean FLACs are no-ops.
- The resulting native FLAC stream is byte-for-byte identical to the wrapped file's inner FLAC stream.
- `AnalysisResult` keeps physical source identity separately from presentation identity, so single-image CUE rows can retain synthetic display names while repair targets the real carrier. Shared carriers are deduplicated before repair, and post-repair refresh inspects the physical carrier for every affected row.

### A1 - exact single-image CUE sample windows

- Seek positions passed to ffmpeg-next are rescaled from samples to FFmpeg's base timebase.
- Decoded frame timestamps are rescaled back to sample positions using the audio stream timebase.
- The first returned frame is trimmed to the exact requested start sample.
- The terminal frame is clipped to the remaining requested sample count.
- The seek fast path remains in place; no temporary extraction was added for seekable carriers.
- Regression coverage compares a seeked second region of a deliberately two-region WAV against the same region analyzed as an isolated file.

### A2 - track-scoped CUE pre-emphasis

- Single-image CUE analysis now derives `FLAGS PRE` from the parsed directives of the specific CUE TRACK row.
- Explicit source tag evidence and catalog evidence remain source-scoped and are still merged.
- The general file-scope CUE detector remains available for ordinary non-CUE-row analysis.
- Regression coverage uses two tracks sharing one carrier with PRE set on only one track.

### A3 - physical source identity for filesystem actions and DR reports

- `AnalysisResult.source_path` is the physical carrier; `AnalysisResult.path` remains the display identity.
- Wrapper repair and refresh use physical provenance.
- ReplayGain writes refuse synthetic single-image CUE rows rather than targeting nonexistent synthetic filenames or inventing per-track tags on a shared image.
- DR reports group by physical source directory, read metadata and codec facts from physical sources, and count a shared carrier's bytes only once.
- Synthetic CUE track labels remain display labels in the report.
- Mixed sample rates, channel counts, bit depths, or codecs report `mixed` rather than copying the first row's facts.
- Unknown or lossy bit depth reports `unknown / not applicable` rather than a decoder-container width.
- Codec reporting uses the probed physical codec, so `.m4a` is not assumed to be AAC when the carrier is ALAC.
- Analysis sorting avoids filesystem/tag-aware sort helpers for synthetic CUE display paths.

### A4 - whole-file HDCD scan duration

- Whole-file HDCD detection no longer passes an implicit one-second `-t` limit to FFmpeg.
- Bounded single-image CUE segment scans retain their explicit `-ss` and `-t` window.
- `ANALYSIS_ALGO_VERSION` is now 27 so stale cached HDCD negatives from the prior behavior miss.
- Regression coverage checks command construction and a deterministic synthetic 16-bit stereo source whose first valid HDCD Format A packet appears after 1.5 seconds.

### A5 - deterministic batch failure disclosure

- Each Analysis run clears and owns an operation-local failure list alongside successful results.
- Failed items remain recorded until the current batch completes.
- Final status reports success and failure counts plus concise failure reasons while preserving successful rows.
- Failure reasons are sorted/deduplicated for stable disclosure.
- Regression coverage proves `Err -> Ok` and `Ok -> Err` completion orders produce the same final status.

### A6 - unknown/lossy bit depth

- Non-integer or otherwise unknown bit depth renders as `unknown / not applicable`, not `0-bit`.
- The fallback HDCD eligibility path requires a known positive integer depth before accepting `<= 16`.
- DR report probing suppresses misleading integer depth for float sources and known lossy codecs.
- Focused helper/report regressions cover formatting, eligibility, and mixed/unknown report summaries.

### A7 - cache-hit pre-emphasis freshness

- Full Analysis cache hits retain cached PCM, loudness, and HDCD results but refresh cheap metadata/CUE/catalog pre-emphasis evidence.
- Sidecar CUE edits therefore take effect on a normal non-forced Analyze without re-decoding the audio file.
- Regression coverage caches an audio result, edits only its sidecar CUE to add `FLAGS PRE`, and verifies the next cache hit reports the new evidence.

## Focused regression coverage retained or added

- Native FLAC wrapper classification: both wrappers, prefix only, trailer only, clean, and ID3-tagged non-FLAC rejection.
- Direct wrapper repair preserves the exact inner native FLAC stream and is idempotent.
- Two-track single-image CUE wrapper repair preserves synthetic display paths, uses one physical repair target, creates no synthetic files, and refreshes all rows clean.
- Cache hits rebuild wrapper-repair provenance from fresh wrapper inspection.
- Exact seek/sample-window equivalence against an isolated region.
- Per-track CUE PRE isolation.
- Synthetic-row mutation refusal and display-only sorting.
- DR report mixed physical facts and unknown/lossy bit-depth formatting.
- Whole-file HDCD command construction and late-code detection.
- Completion-order-independent batch failure reporting.
- Sidecar-only PRE cache refresh.

## Validation performed in this sandbox

The repository's pinned Rust/Nix environment is not installed here. `cargo`, `rustc`, `rustfmt`, `nix`, and `flox` are unavailable, so I did not run or claim the Rust workspace gate. The brief assigns that gate to the operator side.

Available checks completed:

- Full-tree diff against bundled head reconciles to exactly 14 changed files: 12 Rust files plus this delivery note and the changed-files manifest.
- `git diff --no-index --check` reports no whitespace errors for all 12 changed Rust files.
- No merge-conflict markers are present in `src/`.
- Every `AnalysisResult` constructor in `src/` supplies `source_path`.
- The changed implementation paths have an empty intersection with `REFERENCE_COMMON_SOURCE_PATHS`.
- No `crates/tonepoet-true-peak` source changed.
- The shipped wrapped FLAC fixture has a 10-byte ID3v2 prefix and a 128-byte ID3v1 trailer. Removing exactly those wrappers yields a 22,340-byte native FLAC beginning with `fLaC`.
- Wrapped and stripped fixture forms decode to identical `s32le` PCM SHA-256: `ded4df692ca2816f5e918d4bd8ebfbb9b79743dfd7f15144112e3a3837ae8c63`.
- The stripped native FLAC stream SHA-256 is `98b2d651d897cf40deae123f7c15ca4abfae929ed1cd31b5a3ab6e2a5d4a6f6c`.
- FFmpeg reports decoder errors on the wrapped fixture and no decoder errors on the stripped form; `metaflac --list` accepts the stripped form.
- A deterministic synthetic late-HDCD WAV places its first valid packet at about 1.5001 seconds. FFmpeg reports `HDCD detected: no` with `-t 1` and detects Format A with peak extension when scanning the whole file.

Operator-side acceptance remains:

```text
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

Run that command inside the repository's pinned `nix develop` environment. The bundled brief records the starting gate as 7077 passed / 0 failed / 16 ignored with zero warnings.

## Reference qualification decision

No Reference requalification is required for this delivery.

None of the implementation changes touch a path in `REFERENCE_COMMON_SOURCE_PATHS`. The SSRC Binary64 registry, installed Reference report, certification, and evidence remain unchanged. The delivery also does not touch `crates/tonepoet-true-peak`.

## Changed files relative to bundled head

Implementation:

- `src/db.rs`
- `src/flac_envelope.rs`
- `src/tui/analyze.rs`
- `src/tui/app.rs`
- `src/tui/command.rs`
- `src/tui/dr_report.rs`
- `src/tui/draw_overlays.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/preemphasis/metadata.rs`
- `src/tui/preemphasis/mod.rs`
- `src/tui/probe.rs`

Delivery metadata:

- `CHANGED_FILES_analyze_id3_wrapper_repair_2026-09-26.txt`
- `DELIVERY_NOTES_analyze_id3_wrapper_repair_2026-09-26.md`
