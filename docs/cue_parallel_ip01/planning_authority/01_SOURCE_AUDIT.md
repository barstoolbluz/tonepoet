# Source audit — CUE acceptance to visible tracks

**Source:** tracked `tonepoet/` tree in `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` (SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`). All `path:line` anchors refer to this baseline, not to future edits. Analysis is static except an explicitly separated FFmpeg experiment. There are **no measured TonePoet CUE timing results** in this session.

## Principal path: one FLAC+CUE image → separate FLAC files, source rate/depth, no intentional DSP

1. **Queue/association.** `src/convert/queue_expansion.rs:723-945` examines explicit CUEs, suppresses referenced audio queued independently for genuine split sources, and distinguishes metadata-only CUEs describing existing tracks. `:1487-1807` covers synthetic multi-CUE merges; `:1844-2100` embedded CUE precedence/coherence; `:2120-2265` native multi-FILE. `src/convert/pipeline/materializer_cue.rs:798-909` enforces `IgnoreCue`, `SidecarOnly`, `PreferEmbedded`, `PreferSidecar`, `EmbeddedOnly` selection. Source path resolution is in `:1565-1738`; do **not** reimplement from CUE text in a new worker.
2. **Decode-source authority.** `src/convert/pipeline/materializer_cue.rs:491-690` enumerates **unique physical images**, probes each image with `ffprobe` once, reads album metadata and, when preservation is on, artwork, and computes per-FILE track bounds. Probing is normally **header-only**, not a forced full decode (`:1891-1935`). FLAC STREAMINFO has its own exact sample-count parser (`:74-107`); a generic rational `duration_ts` counts as exact only when it maps to a whole sample (`:2183-2310`). The tool must not replace those authorities with a rounded number of seconds.
3. **Track intervals.** `src/convert/pipeline/materializer_cue.rs:2335-2492` partitions samples at CUE `INDEX 01`, uses integer `floor(cue_frames × sample_rate / 75)`, and respects each physical FILE. `src/convert/cue_parser.rs:24-90` retains `INDEX 00`, CUE directive and metadata structures; `:4593-4750` includes EAC noncompliant multi-FILE grammar. Existing track-1 leading samples before INDEX 01 are not automatically emitted as a track; INDEX 00 is not a license for a parallel worker to silently add HTOA or move boundaries. Change only under a separate semantics-correction packet.
4. **Preparation.** If the probe establishes lossless PCM coding, exact samples, known channels, and *not* WavPack, `src/convert/pipeline/materializer_cue.rs:604-690` creates a **descriptive `CueStreamSegment`**; it does **not** decode the whole image or write each PCM segment at that point. Otherwise, a segment is staged as WAV (`CueSegmentCarrier`); lossy last segment has an open-ended tail contract. `:322-465` has a narrower scratch-space preflight for exact FLAC/WAV/RF64; do not equate that preflight's coverage with all possible streaming routes.
5. **Scheduler ownership.** `src/convert/processor.rs:5350-5555` coalesces adjacent `CueStreamSegment`s into a single scheduled group work unit. `src/convert/pipeline/scheduler.rs:1-160` already provides a shared work pool; other queued work can proceed independently. Hence **the whole conversion queue is not intrinsically serial**, but tracks within *this group* do not get separate scheduler workers.
6. **Direct-plan admission.** `src/convert/pipeline/track_executor.rs:1355-1539` asks the existing planner for each member's specialized route. It rejects registered effects (including HDCD), true-peak, common-realizer/references, and commands outside a narrow single-command FFmpeg/SoX encoding shape. A legal output choice alone does not make a CUE track direct-stream eligible. `src/convert/pipeline/stages.rs:45202-45372` preflights every group member; **one rejected member sends its entire physical group through file-backed fallback before launching the direct producer**.
7. **Current serial fast path.** `src/convert/pipeline/stages.rs:44190-44224` launches **one sequential FFmpeg decoder** to raw PCM stdout (no range seek). `:45372-45585` emits exact segment byte spans and invokes `run_segmented_tool_pipeline_with_concurrency`. `src/convert/pipeline/tool.rs:2374-2700` supervises producer and bounded 64-KiB transport, starting **one encoder at a time**; bytes outside selected intervals are discarded, and a final unselected tail can stop the producer early. Therefore full-image decoding from sample zero to the highest selected end is **gated by this route**, not an unconditional multi-pass staging step. Multiple *physical* decoder groups are currently visited serially within a single scheduled album group (`src/convert/pipeline/stages.rs:44413-44943`).
8. **ReplayGain default is contextual.** `tonepoet-pipeline/src/settings.rs:118-145` and `:1736-1765` give the generic API no ReplayGain scan. But the **Convert UI initializes its ReplayGain pill to `Both`** (`src/tui/app.rs:5546-5554`, `src/tui/pill.rs:22-41`), barring subsequent user/preset changes. The direct path has a Unix FIFO source-pass observation that can compute track and album ReplayGain during the same decode (`src/convert/pipeline/stages.rs:44299-44325`, `:44441-44943`), **only when source/plan/fidelity and platform are eligible**. If it cannot mirror, the existing output-measured ReplayGain path is retained (`:15659-15870`). On non-Unix the FIFO setup is unsupported, so assume no source-pass gain without evidence. The actual UI default must be in performance trials.
9. **Outputs and visibility.** `src/convert/pipeline/stages.rs:45044-45200` finalizes each direct output and records executed work; `:2436-2575` applies lossless output sample/depth validation, including FFprobe when count/depth are known. Do not misdescribe this header probe as an unconditional full-output decode. Metadata, native ReplayGain when needed, verification and publication run in existing downstream stages (`:15554-15870`, `:46650-46800`). `:27544-28100` and `src/convert/pipeline/transactional_state.rs:20-128` provide governed staging/publication, including cross-device copy-to-sibling temp, fsync and rename. A worker must hand back **staged artifacts and truthful records**, not directly expose completed track files.

### Source pass and I/O census for the qualified serial route

| Work | Presence/condition | Distinguish from |
|---|---|---|
| FFprobe source header | Expected per physical image on CUE materialization | Full decode: **no** `-count_frames` |
| Read image/embedded metadata and artwork | Conditional on source and `preserve_artwork` | Generic PCM stage: **not necessarily** |
| Single raw source decode | When *whole physical group* is admitted to existing segmented transport | Eager per-track staging: **absent** on this path |
| One encoder per track, sequential | Segmented direct path, planner-authorized | No simultaneous encoder work in one group |
| Per-track PCM WAV materialization | When direct stream not structurally eligible **or** a direct-plan preflight rejects the group | Not unconditional for lossless exact FLAC |
| Extra source-pass ReplayGain observation | Eligible Unix FIFO direct group only | Does not independently require another full source decode |
| Post-encode output measurement | Needed if ReplayGain enabled but source pass unavailable/invalid | May reread decoded output; do not conflate with cheap FFprobe |
| Output sample/depth FFprobe | For qualifying lossless outputs with expected facts | Generally not a full decoded-audio read |
| Metadata/artwork mutation; publish | Conditional settings plus required integrity/publication stages | Track visibility is not encoder-exit time |
| Copy across filesystems | Only when staging and visible destination cannot rename directly | Do not claim every publish is a cross-volume copy |

### Corrected prior claims

- **True:** There is a one-decode/serial-encoders fast path. **False:** every FLAC+CUE necessarily stages PCM WAV for every track.
- **True:** one plan-disqualified member can force whole-group fallback. **False:** it causes *every queued image in the app* to fall back.
- **True:** multi-FILE physical groups run serially inside one combined CUE scheduled unit. **False:** the independent queue is universally serial.
- **True:** existing native ReplayGain, verification, artwork and publication can add elapsed time. **Not proved:** which dominates the reported 2–5× deficit. Historical single-output profiling is **not** multitrack CUE evidence.
- **True:** a current implementation may accept other exact-PCM image formats through FFprobe. **False:** that fact qualifies exact independent seek for those formats.

## Distinct deviations requiring separate eligibility

| Input/operation | Current path / decision | Implication for pilot |
|---|---|---|
| Multi-FILE CUE or independently encoded sources | Canonical FILE resolution and per-file bounds; groups separated by physical path (`materializer_cue.rs:491-690`; `stages.rs:44413-44467`) | Pilot one physical FLAC image only; later multi-file parallelism optional |
| Metadata-only CUE for existing individual tracks | Queue treats as metadata overlay, not image split (`queue_expansion.rs:723-945`; `split_cue_album.rs:604-640`) | Existing worker pool suffices; do not introduce duplicate split |
| Multiple merged CUEs | Queue synthesizes logical album descriptors (`queue_expansion.rs:1487-1807`) | Preserve original authority and album index; no string-based synthetic reparse in workers |
| Partial selected tracks | Exact authored start/end, tail limit; skips not equal full image | No seek outside physical sample total or selecting unrequested tracks |
| A different plan for one member | Group-wide prelaunch fallback (`stages.rs:45202-45372`) | First extension may remove fallback cliff only after per-member equivalence proved |
| Lossy/inexact input | Eager staged WAV, tail determined by decoded samples (`materializer_cue.rs:604-690`) | S3 ineligible initially; sample determinism/codec delay separate |
| WavPack CUE | `.wv` intentionally file-backed; `wvunpack` whole-image fallback if FFprobe fails (`materializer_cue.rs:604-690`, `:1789-1890`) | Later adapter requires separate proof |
| HDCD/de-emphasis/true-peak/gain/DC shift/resample/dither/DSD | Member-aware planner/registered effects and source/album state may be required (`track_executor.rs:1355-1539`) | Explicit S1/file-backed rejection; never silently omit effects |
| Cross-device final location | Governed transactional copy to sibling/temp and rename (`stages.rs:27544-28100`) | Keep original publisher and cancellation/failure semantics |

## Bottleneck hypotheses, prioritized but **not measured**

1. **Likely parallelizable CPU work:** encoder time is serialized within a direct physical group despite independent FLAC track outputs; however source decode must be seeked independently to overlap encoders.
2. **Route cliff:** any one incompatible track can cause whole-group WAV staging and extra source reads. Worth measuring separately, not presumed common.
3. **ReplayGain/output work:** UI defaults to Both; mirrored analysis only on eligible Unix route. New range workers must not accidentally turn one pass into more aggregate work than they save.
4. **Input seek cost:** S3 duplicates decoder startup, file opens and some decode; on HDD or very short tracks S1 may win. Must be measured.
5. **Artwork, probe, publication, cross-filesystem:** real phases, but no measured cost; add narrow timing before optimizing.

## Environment/evidence boundaries

- Source archive integrity independently checked. Exact line ranges are revalidated by `tools/verify_source_refs.py` against this snapshot.
- No Rust toolchain and no production CUE execution in the planning runtime. The attached source-identity letter's 7,575 tests were **not** rerun here. Any current speedup claim is **NOT MEASURED**.
- Exploratory external-tool check only: FFmpeg input-side seek against a **generated 12-second stereo 44.1-kHz 16-bit FLAC** matched a PCM reference for nine non-frame-aligned starts (`1, 586, 588, 589, 1000, 44101, 137137, 220501, 411883`). It does **not** prove arbitrary seek precision, all sample depths, FLAC metadata quirks, backends, platforms, truncated input, or exact output/range termination. IP-02 must qualify them before admission.

**Audit verdict:** do not build a generic format-independent CUE converter. First qualify a narrow seekable FLAC range adapter that calls the **existing** planner/recording/finalization and publication code, with S1 intact as comparator and fallback. Prove ReplayGain and overall elapsed-time performance against actual UI defaults.

## R2 independent review corrections (2026-10-22)

- **Pipeline platform limit, proved in source:** the real supervised two-process `ToolRunner::run_pipeline` exists for Unix (`src/convert/pipeline/tool.rs:2170-2360`) but its non-Unix branch refuses it (`:2361-2372`); the current segmented S1 runner also refuses on non-Unix (`:2775-2787`). A successful *external FFmpeg seek test* therefore **does not** qualify native TonePoet two-process worker execution on Windows. The S3 gate must reject unsupported runner/platform combinations *before* any work starts; retain exactly the preexisting Windows behavior, without claiming that baseline S1 already works there.
- **Reusable global limit, proved in source:** `ToolConcurrencyLimits` already owns shared per-family Tokio semaphores (`src/convert/pipeline/track_executor.rs:709-804`) created once for the processor (`src/convert/processor.rs:2167-2185`). Reference/segmented helpers count a distinct family once (`track_executor.rs:11607-11661`, `:10341-10377`), even when two children of that family are live. Ranged workers must reserve permits for their actual simultaneous child count. This calls for a **small additive S3 permit acquisition** at the existing runner boundary, not a second global RAM/FD/I/O reservation subsystem or a general tool-executor rewrite.
- **Existing wall-time infrastructure:** `ConversionRunTiming` already uses monotonic `Instant` and emits a wall span (`src/convert/pipeline/stages.rs:16541-16565`); scheduler metrics already count work and tool time (`src/convert/pipeline/scheduler.rs:81-149`). IP-01 should reuse this and add only missing narrow CUE route/queue/visibility evidence, not a parallel telemetry model.

These are source facts, not measurements of CUE throughput. R2 changes the **planning contract and bundled scripts only**; the baseline production source remains untouched.
