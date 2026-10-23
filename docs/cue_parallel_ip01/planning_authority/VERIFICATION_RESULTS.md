# Planning-session verification and future per-packet evidence

**Context:** 2026-10-22. Planning-only assessment of original snapshot. This ledger is append-only across implementation sessions; future result rows must bind exactly to the new source archive SHA and tool versions. No production Rust changes or native TonePoet CUE benchmark occurred in this session.

| Check | Outcome | Actual evidence / qualification limits |
|---|---|---|
| Snapshot archive SHA-256 | **PASS** (container SHA-256) | `sha256sum /mnt/data/TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` exactly `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26` |
| Snapshot extraction and source trace | **PASS** (static reading) | Extracted git archive and located parser/materializer/group/transport/ReplayGain/metadata/pub source paths; **does not prove actual runtime performance** |
| Git branch/commit/clean state | **REPORTED ONLY** | Attached `TonePoet_SOURCE_IDENTITY_2026-10-22.md` asserts commit `98aae2b`, branch main, clean, level with origin; the archive has **no `.git`**, so unavailable to confirm independently here |
| Previously run 7,575 tests, zero compiler warnings | **REPORTED ONLY** | Attached source-identity document; these results are not rebound to a new implementation archive and not rerun here |
| Rust Cargo toolchain | **NOT AVAILABLE** | `cargo` and `rustc` are absent from this planning container; compile, clippy and cargo test **NOT RUN** |
| FLAC range experiment | **EXPLORATORY, NOT QUALIFIED** | Generated ~12-second 16-bit 44.1k FLAC and compared FFmpeg input-seek PCM to reference for nine offsets; one tool/environment, not production integration/seek proof. See `01_SOURCE_AUDIT.md`. |
| CUE total wall time, source-to-publish | **NOT RUN / NOT MEASURED** | No actual TonePoet CUE conversion or foobar2000 comparable benchmark in this planning session |
| New parallel worker sample exactness | **NOT RUN** | No worker implementation exists; IP-02 and IP-04 |
| ReplayGain/metadata/failure/pub parallel parity | **NOT RUN** | No production changes; IP-05 and IP-06 |
| Bundle source-reference check | **PASS** (static anchors only) | `python3 -B tools/verify_source_refs.py . /mnt/data/tp_plan_work/tonepoet`: 154 valid referenced line anchors, zero out-of-bounds/missing. This validates file/line existence, **not the source-level claim**. |
| Bundle manifest, links, checksums and DAG | **PASS** | `python3 -B tools/package_bundle.py . /mnt/data/TonePoet_CUE_Parallel_Plan_R1_98aae2b_2026-10-22.tar.gz` ran `tools/verify_bundle.py`, which verified 28 hashed payload files, six committed prompts, acyclic graph, local Markdown links and SHA256SUMS; final archive independently rechecked after packaging. |
| Fixture-generation smoke tests | **PASS** (standalone test tools only) | `tools/generate_cue_fixture.py` generated one-minute 12-track FLAC+CUE stereo 16/44.1 (`7e7dcd2a8c0f33697630d16e79db0ec474d15f368ec7d4059c12c0cf056e9ba6`) and one-minute 3-track FLAC+CUE stereo 24/96 (`898407ee92520c03756c1443ea131959798ae47d65f1e2ba77b0430085a429bb`); FFprobe verified depth and sample/rate; `flac -t` passed on the 16-bit fixture. **These are not TonePoet runs**. |

## Implementation-session result schema (append each packet)

```text
source_archive_sha256:
source_git_commit_or_git_archive_identity:
packet_id_and_prompt_sha256:
host_os_cpu_ram_storage_filesystem:
rustc_cargo_versions:
ffmpeg_ffprobe_sox_flac_versions:
commands_run: [{cmd, exit_code, time, PASS|FAIL|NOT RUN, raw_log_path}]
fixtures: [{sha256, sample_format, cue_description, decoded_reference_sha256}]
claim_to_evidence: [{claim, command_or_log_id, source_and_fixture_sha256, gate_status}]
static_gates:
build_runtime_gates:
performance_gates:
unresolved_failures:
accepted_by_and_date:  # leave blank until actually accepted
```

Never copy a PASS result from an earlier source hash without re-running/rebinding the applicable test. Clearly distinguish static/source inspect, compiled native execution, fixture accuracy and measured elapsed wall-time. No fractional 'mostly pass' relabeling.

## R2 independent adversarial planning review

The R1 source SHA-256 and all 28 R1 payload hashes were verified in the audit environment. The R1 bundle verifier passed, and the source-ref checker reported 154 numerically valid anchor ranges; **this verifies existence, not semantic support**. Script syntax checks and `generate_cue_fixture.py --minutes 1 --rate 44100 --depth 16 --tracks 12` passed with installed FFmpeg. Rust compile/tests and live TonePoet performance **NOT RUN** (toolchain unavailable). See `06_INDEPENDENT_ADVERSARIAL_AUDIT.md`. R2 final file checksums, manifest, archive reproducibility and script smoke tests are verified separately on the delivered R2 release.

R2 corrective smoke test: deterministic archive repack matched byte-for-byte; fresh extraction passed 30-file checksum inventory and 197 source-reference bounds; five synthetic *test-only* paired trial rows with matching configuration hash produced a candidate, while mismatched configurations produced only INSUFFICIENT results. These are unit smoke checks of the calculation tool, **not benchmark measurements**.
