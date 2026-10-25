# IP-04 D-01..D-03 corrective (2026-10-25)

New correction-specific source/test evidence: `CORRECTIVE_D01_D03_2026-10-25.md`. Original acceptance/status restrictions remain: **implemented-unqualified**, S3 OFF, Rust host qualification NOT RUN. To reproduce the new independent gap/reference PCM proof: `python3 -B docs/cue_parallel_ip01/ip04_verification/native_gap_reference_probe.py`; this is not a Rust integration test. The earlier full native 12/30 probe is run **without** `--quick`; `--quick` selects 3/3.

---

# IP-04 private FLAC ranged worker — verification package

**As of 2026-10-24 America/New_York: `implemented-unqualified`, not accepted.**

Exact input: `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz`, SHA-256 `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7`. User-supplied identity letter reports clean host commit `c1103be` and accepted IP-03, including a correction to `ToolConcurrencyLimits::new`. The archive has no `.git` and its commit claim cannot be independently reconstructed from the tarball. Earlier IP-03 in-archive notes saying IP-03 was unqualified are historical; the newer separately supplied identity letter is the authoritative acceptance update.

Read in this order: `STATIC_AUDIT.md` → `VERIFICATION_RESULTS.md` → `CONFORMANCE_MATRIX.md` → `SOURCE_DIFF.json` → `PRODUCTION_DIFF.patch` → `NEXT_SESSION_HANDOFF.md`. Source integrity: `SOURCE_MANIFEST.json`, `SHA256SUMS`, `make_manifest.py`, `verify_static_ip04.py`.

## Private implementation

`run_qualified_flac_ranges_for_group` in `stages.rs` is deliberately never called from the normal CUE scheduler. The caller must supply a typed CUE FLAC group and the already-shared `Arc<ToolConcurrencyLimits>`; every member is preflighted before launch. At most two futures can be in flight per private group, with shared IP-03 weighted semaphores enforcing process caps globally. Each independently prepared IP-02 `CueFlacRangeDecoder` and `ExactPcmExtent` travels through `run_cue_s3_pipeline_if_admitted` without reacquiring tokens. The new Unix `ToolRunner::run_pipeline_with_exact_pcm_extent` uses the same supervised child execution and a fixed 64-KiB shuttle: it refuses surplus *before* forwarding and truncated output at EOF. It has no PCM staging array and cannot silently fall back to the unchecked pipe.

Already-planned encoder commands, metadata obligations, finalization, original source identities, sample/depth validation and execution records are reused. Results are staged only, and sorted to original index on completion. There is no visible publication, ReplayGain/tag policy execution, new user knob, or S1 activation. A failed launched child cannot be retried as S1 here.

## Status and limitations

Native FFmpeg simulation independently decoded and re-encoded 42 exact-ranged tracks with byte-identical decoded PCM against full-image reference: 12 at 16-bit, 30 at 24-bit. Those results **do not run the Rust supervisor**. No Rust compiler, Cargo or Rustfmt is installed in the authoring environment. All named Rust tests are **NOT RUN**; host must compile/fmt/test before declaring IP-04 accepted or enabling IP-05. Native measurements represent the independent Python harness, not actual TonePoet resource or speed data.

Historical IP-03 source-manifest gate is intentionally invalidated because this is a new source revision; it is preserved for its own historical archive and its six Python integrity regression tests still pass. Use **this IP-04** gate for IP-04 source integrity.

```sh
python3 -B docs/cue_parallel_ip01/ip04_verification/verify_static_ip04.py
python3 -B docs/cue_parallel_ip01/ip04_verification/verify_static_ip04.py --baseline /path/to/accepted-ip03/tonepoet
# Native independent fixture (never qualifies the Rust path):
python3 -B docs/cue_parallel_ip01/ip04_verification/native_flac_pairs.py --output /tmp/cue-ip04-native.json
```

`SHA256SUMS` is relative to the **extracted source root**. It lists every regular source file plus `SOURCE_MANIFEST.json`, but never includes itself; `SOURCE_MANIFEST.json` excludes itself and `SHA256SUMS`, and `SOURCE_DIFF.json` compares baselines excluding itself. Run `sha256sum -c docs/cue_parallel_ip01/ip04_verification/SHA256SUMS` from source root. Symlinks are included in manifest with exact targets. A normal non-root `tar -xzf` extraction is enough.
