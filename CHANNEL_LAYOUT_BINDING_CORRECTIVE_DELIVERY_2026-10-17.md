# TonePoet channel-layout binding corrective — engineering delivery

**Source:** user-supplied `TonePoet_channel_dc_FULL_SOURCE_2026-10-17.tar.gz` (the channel/DC implementation baseline).  **Date:** October 17, 2026, New York local time.  **Scope:** only stereo-fold source speaker-role binding and directly necessary execution propagation. Not a scheduler, matrix, DC removal, resampler, or terminal redesign.

## Defect and correction

The existing FFmpeg `pan` matrix resolves positions by speaker role. A SoX-written Float64 WAV or raw Float64 intermediate may not retain the original WAVEFORMATEXTENSIBLE channel mask. FFmpeg infers a default from channel count and may interpret 3.0 as 2.1 (silencing center-only audio) or quad as 4.0 (misrouting rear-left). The former code passed `-ac` but not the role layout at the input boundary.

The correction:

1. Requests `stream=channel_layout` from the **existing** source `ffprobe` realization, before effect/intermediate creation. For multichannel WAV/RF64/BW64, the original WAVEFORMATEXTENSIBLE `dwChannelMask` is required and preferred over decoder defaults. Generic/unmasked multichannel WAV is not evidence of speaker roles and is refused **only when stereo fold is selected**.
2. Accepts a decoder-reported layout for speaker-aware compressed codecs (including FLAC and WavPack), with a recognized alias/hex mask, supported physical speaker roles, and an exact popcount-to-channel-count check. The source codec whitelist deliberately excludes generic PCM that can inherit FFmpeg's count-only guesses. Rejects genuinely missing/unsupported/mismatched multichannel identity with a useful message; channel-preserving conversion and DC filtering are not affected.
3. For DVD-Audio sources realized as unmasked WAV, uses only the existing validated DVD-A assignment and matching output-order policy where it admits a safe FFmpeg mapping. SACD ISO materialization is bound to the declared channel count and the built-in writer's known surround ordering. Other unknown source families fail closed if the original role assignment cannot be established.
4. Threads a verified `0x...` speaker mask into the **existing** registered-effect chain. When lowering `StereoFold`, FFmpeg receives `-channel_layout <verified_mask>` **before** `-i` whether the carrier is headerless f64le or Float64 WAV. The rest of the effects retain their existing lowering and receipts. After a fold, subsequent effects use `0x3` (stereo) instead of the input's multichannel mask. Mismatched channel geometry cannot be submitted as a fold.
5. Also forwards the original layout into the DSD certified true-peak **separate** reconstruction/observation path so a folded DSD carrier is not reinterpreted by count after reconstruction. The PCM true-peak path already uses the registered-effect carrier route for these effects.

No change to normalized coefficients, LFE exclusion, source/target channel choices, dither ownership, fingerprint semantics, or logging architecture. No additional external dependencies.

## Verification performed in this environment

- `python3 validation/channel_dc_smoke.py`: **13 PASS** (existing multichannel, LFE, DC, SoX/FFmpeg smoke checks).
- `python3 validation/channel_layout_binding_corrective.py`: **14 PASS** independent decoded-audio cases, using real FFmpeg/SoX and the *literal production pan expression*: center-only 3.0, LFE-only 2.1, quad RL-only, 4.0 BC-only, back and side 5.1, mono/stereo, two headerless Float64 cases, and actual 3.0/quad FLAC and WavPack decoded through a SoX Float64 WAV. Direct source reference vs role-bound intermediate match within the test tolerance; unbound center-only Float64 WAV independently reproduces silence.
- `git diff --check`: PASS.
- The added Rust tests cover source masks vs guessed layouts; 3.0/2.1, quad/4.0 and 5.1 role identities; FLAC/WavPack cases; SACD assignment; input option placement; and refusal without a verified binding.

**Qualification not performed:** There is no `cargo`, `rustc`, or `rustfmt` in this offline container. No Rust compilation, test suite, clippy, official complete conversion run, exact true-peak/dither end-to-end run, or Reference qualification has occurred here. The reported 7,529 passed tests belong to the *input baseline*, not this correction. This is an implementation candidate pending build-host acceptance.

## Build-host acceptance (bounded)

1. From the unpacked repository root, run its documented environment/toolchain gate and `cargo test --workspace` (and `cargo check --workspace` where normally required). Check formatting of touched Rust code with the project's formatter; avoid unrelated reformatting. Record baseline comparisons.
2. Convert a **center-only three-channel FLAC** with `Channels: stereo`, and verify both stereo channels retain nonzero center content, including a SoX Float64 intermediate; do the same for rear-left-only **quad FLAC** and WavPack (left only). Run **2.1 LFE-only** (zero fold output), distinguish quad from 4.0, and retain existing mono/stereo/5.1(back)/5.1(side) conversions.
3. Exercise one real headerless Float64 carrier, one stereo fold combined with certified true-peak (including DSD if supported), and one ordinary final PCM bit-depth reduction using terminal dither; verify audio geometry, actual invoked commands, execution records, and dither location.
4. Supply a deliberately unlabeled three-/four-channel WAV and verify stereo folding **refuses without processing**, with an informative error, while ordinary channel-preserving conversion/optional DC removal still work.
5. Re-run the repository's existing release/Reference gate on an eligible build host; do **not** infer certification solely from the DSP smoke tests.

## Explicit boundaries

- Not all multichannel formats carry trustworthy speaker identities. Refusal of a fold without them is intentional and preferable to silent corruption. Some decoded disc sources may require an explicit supported source mask to opt into folding; do not invent one from channel count.
- Existing unsupported certified SSRC combinations are unchanged; qualifying new replay families is outside this corrective.
- Separate DSD album failure-accounting receipts discussed in the audit are **not changed**, as they are unrelated to the role-routing defect.
- No attempt was made to implement a second positional SoX folding backend, to add a DSP scheduler, or to change true-peak policies.
