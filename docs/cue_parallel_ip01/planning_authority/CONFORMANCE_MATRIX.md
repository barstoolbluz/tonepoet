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
