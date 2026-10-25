# CUE parallel conformance evidence ledger

**Statuses for every cell:** `NOT RUN`, `PASS`, `FAIL`, `BLOCKED`, or `NOT APPLICABLE`; a code route found by static reading is **NOT** a qualified source/format test. Source audit rows are 'observed source structure' only. Each verified cell must identify cumulative archive SHA, machine/OS, native decoder/encoder version, actual command, fixture SHA, expected PCM sample count/hash, test evidence file and claim it supports.

| Capability / policy | Baseline source structure | Parallel runtime certification | Activation |
|---|---|---|---|
| FLAC image → FLAC track, no DSP, s16/44.1 | Source routes to `CueStreamSegment` if exact facts; S1 gated by planner | **NOT RUN** | S3 disabled |
| FLAC image → FLAC track, no DSP, s24/44.1-96 | Same conditional structural route, carrier s32le | **NOT RUN** | S3 disabled |
| Decoder sample-exact random range and EOF | No independent range command in S1 | **NOT RUN** | IP-02 hard gate |
| ReplayGain Both source/output parity | UI default Both; Unix S1 mirror optional; native output reader exists | **NOT RUN** | IP-05 hard gate |
| Multi-FILE CUE and EAC special | Current physical FILE authority/cue parser present | **NOT RUN** | S1/fallback |
| Metadata-only existing tracks | Queue overlay route | **NOT RUN** | Unchanged |
| HDCD/de-emphasis/true-peak/other effects | Planner direct gate excludes nonqualified registered effects | **NOT RUN** | S1/file-backed |
| WavPack image | Dedicated file-backed and `wvunpack` probe fallback | **NOT RUN** | No S3 |
| WAV/RF64/AIFF/Wave64 | CUE materializer/probes subject to exact sample facts | **NOT RUN** | No S3 |
| ALAC/APE/TTA/Shorten | Codec classifier recognizes some as PCM; CUE viability conditional | **NOT RUN** | No S3 |
| MP3/AAC/Opus/Vorbis | Lossy image staged, decoded length tail authority | **NOT RUN** | No S3 |
| DSD/disc specialized | Separate source kind/DSD coding | **NOT RUN** | Outside pilot |
| Weighted global tool/FD/RAM/I/O ceilings | Tool family semaphores exist but current pipe family counted once | **NOT RUN** | IP-03 hard gate |
| Cancellation, orphan-free cleanup and partial failure | Existing S1/staging transaction semantics | **NOT RUN** | IP-04/05 hard gate |
| Album transaction and cross-FS publication | Existing album publisher / file transaction | **NOT RUN** | IP-05 hard gate |
| End-to-end net wall-time with UI RG Both | **Unmeasured** in this source inspection | **NOT RUN** | IP-06 hard gate |

For expanded format inventory and the source-grounded **current routing facts** see [04_FORMAT_CAPABILITY_MATRIX.md](04_FORMAT_CAPABILITY_MATRIX.md). Do not turn this ledger's source-code observations into PASS claims.


## IP-02 evidence update, 2026-10-24 (decoder feasibility, NOT production certification)

| Scope | Status here | Evidence | Production admission |
|---|---|---|---|
| Standalone exact 16/24-bit FLAC ranged PCM vs S1 baseline for 44.1/48/96k mono/stereo, SEEKTABLE/no SEEKTABLE | **NATIVE TEST PASS** (selected installed executable/version only) | `docs/cue_parallel_ip01/ip02_verification/native_host_results_*.json`; full SHA-256 fixture and reference evidence | **S3 disabled** |
| Independent byte-count verifier including early EOF, surplus and overflow refusal | **STATIC SOURCE IMPLEMENTED**; Rust unit tests **NOT RUN** | `cue_flac_range.rs`, named `cue_ip02_*` tests | Requires Cargo compilation/test and wiring to supervised pipe in later packet |
| Corrupt FLAC frame CRC and truncated read failure | **NATIVE TEST PASS** with `crccheck+explode`; `explode` alone **FAILED** exploratory adversarial probe | Native evidence/error records | Fail closed; keep CRC requirement |
| Long/no-seektable late-offset seek scaling, slow consumer, cancellation/reap | **NATIVE TEST PASS** in limited synthetic host cell; not a portable performance guarantee | Native timing/cancel results | Build-host retest required; no S3 worker yet |
| Real TonePoet S3 worker, resource concurrency, ReplayGain Both, logs/publication parity | **NOT RUN / NOT IMPLEMENTED** | IP-03 through IP-06 | **S3 disabled** |

This table appends evidence without overwriting the historical planning-state table. No transfer of a PASS claim between source hashes, executable versions or machines is authorized.
