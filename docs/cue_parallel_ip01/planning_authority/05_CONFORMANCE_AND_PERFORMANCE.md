# Conformance, failure injection and performance acceptance

**Scope:** equivalence of the new *FLAC+CUE image → individual FLAC* route to **this project's** existing qualified S1/file-backed behavior, plus an independently justified end-to-end speed advantage. This is a test **protocol**, not a claim that any of these tests passed. Pilot remains disabled in production pending evidence.

## 1. Controlled fixtures and reference authority

Use generator-created deterministic multichannel 16-bit and 24-bit PCM with index-derived checksums, explicitly named provenance and random bit patterns, plus independently curated real-world fixture classes. Preserve decoded reference PCM per selected track **before** introducing new worker routes. Keep fixtures and their SHA-256s in the latest cumulative bundle or a referenced content-addressed fixture pack. No fixture hash is assumed in this plan.

| Fixture | Requirement and comparison |
|---|---|
| A: 12-track 60-minute stereo FLAC+CUE at 44.1 kHz / 16-bit | Primary unchanged-rate/depth benchmark. CUE indexes integral CD frames; compare reference decoded PCM per track, output sample count, tags, artwork and RG Both. |
| B: same total duration, 1, 3 and 30 tracks | Tests seek overhead, short-track crossover, queue startup. Add 12 tracks with 2-second bursts and long final track. |
| C: 24-bit 44.1/48/96 kHz FLAC, mono/stereo and available 5.1 | Sample carrier promotion s32le, channel order and count. Reject unqualified multichannel subtypes explicitly. |
| D: boundaries inside compressed codec blocks | Offset 0, 1, 2, 587, 588, 589 at 44.1 kHz, 74/75-frame crossing, first/last sample, non-frame-aligned point, empty/invalid range refusal, very long and sparse boundaries where feasible. |
| E: FLAC with/without seektable, unusual metadata, high-padding headers, corruption/truncation | Prove seek/preroll behavior and fail-closed decoder errors; verify no 'success' from approximate seek. |
| F: CUE INDEX 00 vs 01, first pregap/HTOA, EAC noncompliant multi-FILE, multiple physical FILE, merged CUE, metadata-only split tracks | **Reference behavior** not silently altered: exact outputs, source/physical file identity, titles/track ordinal. First release must reject or use old path for unqualified layouts. |
| G: per-track HDCD/pre-emphasis/de-emphasis/true peak/resample/dither/gain/DC/album operation | New route does not accidentally bypass any effect; whole group falls back per current rule unless separate equivalence gate passed. |
| H: both failure policies + cross-filesystem publication | Same final visible state, cleanup, partial acceptance and user diagnostics as baseline. |

**Mathematics:** For CUE time position `F` frames (75 frames/sec), convert as existing implementation does: `floor(F × sample_rate / 75)` with checked `u128` intermediate or equivalent checked existing arithmetic; process exact half-open interval `[start_sample, start_sample+samples)`. Never convert a decoded boundary from floating timestamp and assume exactness. For test vectors compare decoded audio in a canonical high-fidelity integer representation **without lossy re-quantization** (e.g., source 16-bit/24-bit PCM normalized losslessly to exactly the same signed sample width and channel order) and require zero first-difference for qualified untouched PCM route. Check content hash, sample total and boundary in addition to container validity. Don't require the encoded FLAC bitstream to be byte-identical across encoder tools/settings.

**Track/album metadata:** compare CUE titles, performers, composers, dates, numbers/disc numbers, `INDEX 00/01`, language/extra and duplicated/multi-valued tags, artwork bytes/mime when current policy preserves, canonical physical source path, selected-track ordering and provenance. Preserve source/sidecar/embedded precedence, selection and original authored CUE identity. RG Track and Album values must be derived from the same decoded PCM and album membership with native current semantics. Compare tag fields and native scan/reference values; if implementation noise yields rounding differences, document a **source-grounded** permitted tolerance approved during qualification rather than inventing one. Test `ReplayGainChoice::{Off,Track,Album,Both,*IfMissing}` and retained-tag policies, with `Both` the **primary UI default**; API default None separately.

## 2. Fault matrix and boundedness

Inject errors **before spawn**, after some bytes, after a complete member, during encoder close, during output probe, gain analysis, artwork mutation and atomic publication. For every injection capture which child commands started/returned, stdout/stderr truncation markers, original member IDs, whether outputs passed validation, scratch-tree contents, retries and final visible location:

1. **Spawn/seek/probe failure**: rejects before activation or records a failed attempt with no invisible output published as success.
2. **Truncated FLAC, decoder zero/nonzero failure, short or extra PCM bytes**: detects exact count mismatch and terminates encoder; cannot silently pad or shift adjacent track.
3. **Encoder refuses stdin, exits early, pipe broken, disk full, output permission error**: no stdout writer remains blocked indefinitely; collect both exit codes and diagnostic records.
4. **Cancellation at admission / seek / mid-pipe / gain / publication**: bounded time-to-stop; all child processes are reaped; all tokens and descriptor handles released; stop signal cannot race into apparent success.
5. **Slow reader/back-pressure and 200 very long tracks**: live PCM bytes bounded by admitted `workers × buffers`, not album duration/track count; no unbounded Tokio/OS tasks or descriptor growth.
6. **Two simultaneous albums plus unrelated queue jobs**: **shared** process/RAM/FD/I/O budget never exceeded; no nested tool permit deadlock, starvation or resource theft. Request over capacity must select S1 or remain queued according to proven policy.
7. **One member's direct plan fails**: exactly the prelaunch fallback semantics currently in production; a child failure **after launch** must not invisibly trigger side-effecting retry.
8. **AllowPartialAlbum vs fail-closed**: preserve each policy exactly, including whether successfully staged tracks can become visible; ordering and human/log reports keyed to original index.
9. **Same output paths, second run, replay, stale scratch, crash/restart and different filesystem**: no half-published album, stale hidden files, overwrite of unrelated output, duplicate publication or mismatched manifest/record. Album- and track-safe transactional publication remains sole authority.
10. **Metadata/artwork edge cases**: multi-value keys, malformed tags, huge artwork and generated CUE synthetic names cannot contaminate one track's records or another physical source.

A test with a fake ToolRunner is useful for orchestration, but cannot replace a real FFmpeg/SoX/decode-host integration test, especially for EOF, child lifecycle or sample exactness. Test Linux/macOS and Windows or clearly restrict platform admission to actually qualified hosts. Unix FIFO RG and non-Unix output-analysis paths must be independently accounted for.

## 3. Observable phase/identity schema

Use opt-in tracing via existing project logging/telemetry; no new default user data collection. Every event has `run_id`, `physical_source_id` (safe per privacy policy), `cue_album_id`, `track_id`/ordinal or album scope, route (`S1`/`S3`/`fallback`), plan/admission reason, monotonic elapsed time and `stage`:

`action_accepted -> queue_enqueued -> queue_activated -> cue_probe_metadata -> plan_admitted/rejected -> decoder_spawn -> first_pcm -> encoder_spawn -> encoder_exit -> pcm_count_verified -> metadata -> replaygain -> verify -> publish_start -> visible -> cleanup`. Record source reads/temp bytes, output copy bytes, child process count and high-water RAM/FD/pipe-buffer occupancy when supported. **One wall-clock total** from acceptance to visible result; child wall times may overlap and MUST NOT be summed. Telemetry is race-safe, sampled/opt-in, redacts raw source paths when log privacy policy requires, and never holds stream data in memory. Distinguish a worker result from the final album-published state.

## 4. Reproducible timing matrix and decision rule

**Workloads:** identical 60-minute decoded PCM program divided into 1, 3, 12 and 30 tracks; 44.1-kHz 16-bit main run; 24-bit/96-kHz stress. Synthetic noise is good for sample signatures but **not by itself representative of FLAC encoding CPU cost**: include at least one real or realistically compressible music-like album in the automatic-promotion performance cohort, using a lawful fixture the build host already has. One image vs two images and HDD/SSD where hardware exists. For every run record:

- Tool and binary versions (`ffmpeg -version`, `ffprobe -version`, SoX, FLAC encoder), full TonePoet tree/commit/hash, build features/profile/release, host OS/kernel/CPU cores/threads/RAM, storage/model/filesystem and temp/destination paths, CPU frequency governor/thermal state if controllable.
- Explicit ReplayGain `Both` and `Off` results; preserve artwork on/off separately; one nonstreaming DSP scenario to verify S3 rejects; user profile/presets, FLAC compression level/encoder backend and verification toggles **identical** for S1 and S3.
- Fresh and warmed filesystem cache as **separate cohorts**; avoid comparing S1 cold versus S3 warm. Randomize/counterbalance execution order in pairs and record ambient load.
- At least **five complete paired trials** per important cell on same host; median, min/max, sample-to-sample variance and bootstrap 95% CI when enough pairs. A matched S1/S3 pair must have the same **configuration SHA-256** over a canonical manifest excluding route but including binary/build identity, backend/tool versions, compression level, toggles, output/staging policy and other benchmark-affecting settings. `tools/evaluate_trials.py` requires this field; never mix unlike runs into a success result. Record total wall time (`action_accepted` → visible), active source/encoder wall periods, overall audio-minutes per wall-minute, peak RSS, peak scratch bytes, final output sizes, output file/PCM hashes, failure/cancellation outcome.
- Preserve actual raw JSONL timing records and human-controlled fixture/source hashes with the packet output. If an upstream comparison is available, benchmark foobar2000 on identical program, codec/depth, replaygain and metadata/validation toggles; otherwise **do not block** the source-grounded S1-vs-S3 gate.

**Hard go/no-go**:

1. **Correctness first:** zero qualified sample mismatches, zero output count/channel mistakes, no policy/provenance/pub/cancel regressions, no budget violations. Any fail **blocks** promotion; do not trade fidelity for throughput.
2. **A credible benefit:** on a representative SSD/CPU cohort with at least 12 medium tracks and the **default RG Both**, aim for a repeatable `S1_wall/S3_wall ≥ 1.25` median and supportive paired evidence; compare also RG Off. This is a **decision threshold, not a predicted speedup**. Establish actual confidence and resource footprint rather than claiming success from a lucky run.
3. **No common-case harm:** automatically admit only workload classes whose measured crossover is favorable; small/short albums should remain S1. Reject auto-promotion for a qualified family where representative median slows >10%, or where tail latency, memory, FD or SSD/HDD traffic increases disproportionately, pending an approved policy.
4. **ReplayGain attribution before rejection:** S1's direct route can observe ReplayGain during its existing decode when the source-pass predicate holds; an initial S3 worker cannot, and pays an extra post-output scan under the default `Both`. Record per run whether S1 **actually** used source-pass analysis, report the RG-Off and RG-Both speedups separately, and account for S3's post-output scan as its own measured interval. A loss attributable primarily to that scan is **not** evidence that ranged decoding and parallel encoding are slower; it holds the pilot open for the `PX-C` source-pass observer and a repeat qualification, rather than rejecting S3.
5. **Insufficient evidence:** hold an explicit opt-in S3 path with S1 default, or reject S3 if semantics cannot be established. Performance **NOT RUN** or built **NOT RUN** never yields automatic promotion. No forced source-pass RG loss to make default-off timing look favorable.

IP-01 creates the baseline observation and recipe, IP-02 certifies raw sample-range truth, IP-03 certifies budgets, IP-04 certifies private workers, IP-05 certifies the public integration, IP-06 records actual decision. `VERIFICATION_RESULTS.md` records each command, exact artifact hash, environment and PASS/FAIL/NOT RUN. `CONFORMANCE_MATRIX.md` records which capabilities were actually admitted.

### R2 prelaunch portability test

The real two-process pipeline runner currently refuses non-Unix execution (`src/convert/pipeline/tool.rs:2361-2372`). Add a fake production-admission test for `runner_pipeline_supported=false` / non-Unix: S3 must not start any child and must preserve the preexisting route and failure semantics. A native production smoke test on each *admitted* platform confirms supervised two-child piping and cancellation, separately from stand-alone FFmpeg range-sample tests. Do not require implementing Windows piping for the initial Unix-qualified pilot.
