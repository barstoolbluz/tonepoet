# DVD-Audio stereo-fold channel order — focused corrective delivery

Date: 2026-10-17 (America/New_York)

## Verdict and reproduction

**Confirmed defect.** The previous stereo-fold implementation established a
verified *mask* at FFmpeg's pan input, but could not prove that DVD-Audio LPCM
*sample positions* agreed with that mask. The DVD-Audio decoder's default
`PreserveDvdAudio` policy can preserve native group order; FFmpeg's s32le WAV
muxer can then write a guessed default WAVEFORMATEXTENSIBLE mask.

With the production pan expression, independently executed with FFmpeg:

| DVD-Audio assignment | Preserved native order misread as WAV | Corrected WFX sample order |
| --- | --- | --- |
| 3, quad, rear-left-only | L=0.14417 / R=0.14417 | L=0.18640 / R=0.00000 |
| 19, 5.0, center-only | L=0.00000 / R=0.13180 | L=0.13180 / R=0.13180 |
| 4, 2.1, LFE-only | L=0.00000 / R=0.00000 | L=0.00000 / R=0.00000 |

The different left amplitudes in the quad case arise from the matrix's
different center and rear-left weights; it is not an error in the matrix.

## Implementation

**`src/convert/pipeline/stages.rs`**

- On DVD-Audio realization **only when `StereoFold` is registered**, select the
  existing `DvdaChannelOrderPolicy::WaveExtensible` policy. The in-process LPCM
  unpacker applies the existing `source_to_output_indices()` sample permutation;
  `mux_s32le_to_wav()` then describes that reordered PCM correctly at the input
  to FFmpeg.
- For DVD-Audio stereo folds, resolve the declared assignment from the typed
  `TrackSourceRef` (falling back to existing source metadata), detect conflicting
  declarations, require matching channels, and require a real WFX mask equal to
  the expected *wave-ordered* speaker mask. Do not substitute ffprobe's
  count-inferred layout or the environmental default source-order policy.
- Non-DVD source-layout admission and the physical FFmpeg pan/lowering are
  unchanged. No new scheduler, effect, channel matrix, or DSP backend.

**`src/convert/pipeline/dvda_realize.rs`**

- Isolate reordered LPCM cache from the previously generated source-ordered WAV:
  default filename unchanged, WFX policy receives the `_wfx.wav` suffix. The
  corresponding cache lock naturally follows the distinct path.
- Validate **both cache hits and new output** under the selected channel-order
  policy. Existing env-selected WFX use also gets the appropriate cache name,
  avoiding accidental cross-policy carrier reuse.
- Preserve prior policy and channel positions for DVD-Audio conversions without
  stereo fold. The MLP branch continues using its existing FFmpeg decoder and
  normal WAV role mapping; no speculative MLP reorder is added.

If a DVD-Audio assignment is missing, inconsistent, or lacks a supported safe
WAV alias, the *fold* is refused instead of guessing. PCM without fold and DC
removal remain admissible as before.

## Validation actually performed (this container)

- `python3 validation/dvda_stereo_fold_corrective.py` — **3 PASS** synthetic
  decoded-sample DVD-A reproduction cases. It uses the literal production pan
  expression and real FFmpeg/SoX processes. It tests source-ordered WAV,
  WAVEFORMATEXTENSIBLE-ordered WAV, **actual header masks**, Float64 WAV through
  SoX, a second consumer of the carrier, and unchanged channel-preserving
  sample positions. The test is **not** an integrated Rust/DVD disc run.
- Existing `validation/channel_layout_binding_corrective.py` — **14 PASS**.
- Existing `validation/channel_dc_smoke.py` — **13 PASS**.
- The repository's standalone `foo_input_dvda` C++ reference-vector generator
  built with `c++ -std=c++17 -O2`: independently checked all **18 vectors for
  assignment 3** and **18 vectors for assignment 19** against the existing
  source-to-WFX positional permutations (**36 PASS**).
- `git diff --check` — PASS. Artifact checksums adjacent to the archives.
- Added Rust regression tests exercise DVD-specific mask/assignment refusal,
  quad and 5-channel index ordering, policy selection, and cache namespace.

**Not run:** `cargo`, `rustc`, `rustfmt`, integrated DVD-Audio PCM decode through
TonePoet, full Rust workspace tests, official reference qualification. This
container has **no Rust toolchain**. The original 7,529-test baseline was from
*before* these changes; do not report it as the current result.

## Build-host acceptance

1. On a host satisfying `Cargo.toml`'s `rust-version = "1.93"`, run
   `cargo check --workspace`, then the repo's documented gate:
   `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`.
   Run the package's normal format checks and record failures separately from
   the baseline. The two affected Rust files contain new focused unit tests.
2. Convert real DVD-Audio LPCM assignment **3 (quad)** with only the rear-left
   active, and assignment **19 (L,R,Ls,Rs,C)** with only the center active.
   Verify decoded samples after a SoX Float64 intermediate. Also check 2.1 LFE
   exclusion, 5.1(back)/5.1(side), terminal dither, and certified true-peak
   where those combinations were already supported.
3. Re-run one track **with a source-ordered cached WAV already present**. Its
   WFX realization must not use that cache, and repeating the WFX conversion
   must retain exactly the same channel assignment and decoded sample output.
4. Verify a multichannel DVD-Audio track with genuinely absent/contradictory
   assignment refuses stereo folding clearly, while channel-preserving
   conversion and DC filtering remain functional.
5. Run the existing qualified build-host Reference/release gate if applicable;
   do not infer qualification from the FFmpeg/SoX smoke tests.

## Non-goals

No change to coefficients or headroom, the registered-effect scheduler,
DC removal, true-peak policy, dither, terminal ownership, SSRC replay,
transaction handling, or unrelated source-family conversion behavior.
