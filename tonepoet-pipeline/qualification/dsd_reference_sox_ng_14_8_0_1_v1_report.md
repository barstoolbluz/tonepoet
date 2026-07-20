# DSD Reference Policy v1 Qualification Report

**Policy:** `sox_ng_14_8_0_1_v1`
**Report schema:** `tonepoet-dsd-reference-policy-qualification-report/v1`
**Policy evidence result:** PASS
**Implementation certification:** requires the mandatory build and real-tool gates below; this source-controlled report is not a substitute for executing them against a compiled checkout.

## Bound evidence

- `docs/tonepoet_dsd_to_pcm_guidance_evidence_based_v9.md` — SHA-256 `a5a5556c70b93c56d216c0d142ab5213920fa9f696caa48ea4110f382bf2e36f`
- `docs/sox_ng_dsd_decimation_test_report_v5.md` — SHA-256 `af6e6880003f2b3673d804b992a093700cd8141465ee0277f8689d48209055c7`
- `docs/brief_dsd_reference_p0_scope_and_commission.md` — SHA-256 `87612a3b1d46aa6e7c4dd34bc9d5f9a45d539aa892ec7b454b52c6d9926288f7`
- SoX-ng source revision `324b8cf873fd7836e8848bd87f7a90d8faa6f849`, resolved by the repository lock
- FFmpeg policy package `ffmpeg_7-full`, resolved by the repository `nixpkgs` lock
- `sacd-rs` source identity and the complete source-controlled DST corpus identities embedded by `crates/sacd-rs/build.rs`

## Qualified policy matrix

The immutable matrix covers DSF/DSD, DSDIFF/DSD, DSDIFF/DST, SACD DSD, and SACD DST; DSD64/128/256; mono and stereo; Reference and Wideband selections according to the frozen profile table; all four gain modes; and the seven P0 lossless target families at their permitted terminal depths and compression levels.

The expanded supported-cell count is `53,760`; the canonical expanded-cell digest is `23571712e01ba8b27c62ebe24930036d9e27e46f41a9e5263955b864ac8ce452`.

Unsupported rates, channels, profile cells, target/depth pairs, lossy delivery, Manual workflows, programme-wide processing, B6 execution, DSD512/1024, and noncanonical container flags remain deterministic pre-render rejections.

## Reconstruction response

The D1 correction is frozen: the SoX-ng `sinc` frequency token is the transition's −6 dB center, `passband + transition / 2`. The centers are B3 30,000 Hz, B4 37,500 Hz, B4W 42,500 Hz, B5 59,000 Hz, and fixture-only B6 114,100 Hz. B1 and B2 use only the integrated `rate -u` response.

The mandatory real-tool target measures B1/B2 integrated response and W64 bridging. For B3/B4/B4W/B5 and fixture-only B6 it measures the complete commissioned `rate -u` followed by corrected `sinc` chain from the applicable DSD source rate, including passband, center, and stopband behavior. Each high-rate fixture is deleted immediately after its steady-state measurement. The gate fails rather than rewriting policy data when a realized response misses its frozen bounds.

## Gain, analyzer, terminal, and packaging authority

The policy uses `DbNano`, checked fixed-point arithmetic, one-sided analyzer bounds, one gain binding, one terminal realization, and a post-final true-peak acceptance measurement. Terminal-error bounds are keyed by policy, target rate, depth, realization, linear error ceiling, and safe pre-terminal ceiling; no floating-point runtime derivation is authoritative.

The real-tool target uses the `ffprobe` executable from the same compiled FFmpeg store closure to assert exact container, codec, rate, channels, integer/float class, and terminal depth for every supported package family across all ten target rates, mono/stereo, every permitted depth, FLAC levels 0–8, and WavPack levels 0–3. WavPack Int24 packaging includes the policy-frozen `-bits_per_raw_sample 24` declaration; without it, FFmpeg can promote a 24-bit input to a 32-bit WavPack stream. The gate requires `ffprobe` to report 24 authoritative raw bits. It then decodes and hashes every sample and repeats sample-identity verification after metadata mutation. Ordinary RIFF admission uses a conservative proof over exact PCM payload size plus a frozen 65,536-byte muxer-structure upper bound, four-times serialized metadata expansion, and—when source-derived tags or artwork are requested—four times the verified container's complete non-audio region.

## DST authority

The P0 corpus binds:

- three commissioned compressed DSD64 stereo `sacd_extract` oracle pairs;
- four six-channel decoder-only cases: three commissioned compressed DSD64 oracle pairs and one independent standards-literal raw DSD64 geometry fixture;
- standards-literal `DSTCoded=0` fixtures for DSD64 mono/six-channel, DSD128 mono/stereo, and DSD256 mono/stereo;
- a source-controlled independent raw-frame oracle that validates the zero header and decodes the payload without importing or invoking the production Rust codec.

Every input and expected output is pinned by SHA-256. Six-channel decode remains decoder qualification only; Reference admission rejects channels 3–6 with `DSD-REF-P0-005`.

The inherited commission did not retain the historical external `sacd_extract` executable hash or commit. This report does not invent it: the accepted commission statement and content-addressed input/output pairs are the retained authority.

## Immutable runtime toolchain binding

The Nix package compiles the exact SoX-ng and FFmpeg store paths into the executable. Runtime activation requires the tool runner path, activation path, and compiled store executable path to canonicalize to the same object; executable hash, reported version, package/store identity, behavior probes, platform ABI, CPU dispatch, `sacd-rs` build identity, fixture identities, policy digest, and semantic plan all enter native-v2 execution identity. Reference never falls back to a different binary or decoder.

## Mandatory implementation-certification gates

A releasable checkout must pass, without weakening or skipping any candidate cell:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The final command atomically writes a machine-readable qualification report containing canonical executable and store paths, executable hashes, reported versions, platform identity, policy-manifest digest, B1–B6 response measurements, in-process backend and DST corpus identities, analyzer and terminal bounds, RIFF capacity authority, supported/rejected matrix counts and digest, all 840 package/decode-back cases, post-metadata sample identity, and the pass/fail outcome. Any compiler, clippy, workspace-test, or real-tool failure remains a release blocker.
