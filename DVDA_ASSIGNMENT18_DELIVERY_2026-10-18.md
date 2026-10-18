# TonePoet DVD-Audio assignment 18 — narrow corrective delivery

Date: 2026-10-18. Baseline: `TonePoet_DVDA_stereo_order_corrective_FULL_SOURCE_2026-10-17.tar.gz`, SHA-256 `203589445803fbde82b7b0c1d1c8d74ea3a9b3f72f317abf53520b03b6f3d1c1`.

## Verdict

Confirmed, reproduced speaker-role defect. DVD-Audio assignment 18 is source order `L,R,Ls,Rs,LFE`. Existing `WaveExtensible` sample reordering correctly produces `L,R,LFE,Ls,Rs`, but the previous table returned FFmpeg `4.1` (WAV mask `0x10f`: `FL,FR,FC,LFE,BC`). Correct layout is mask `0x3b` (`FL,FR,LFE,BL,BR`). Mislabelled source WAV makes the stereo pan mix LFE into both channels, mute rear-left, and mix rear-right into both channels.

## Code changes (bounded)

1. `src/convert/pipeline/dvda_channel_layout.rs`: code 18's `ffmpeg_wave_input_layout()` now returns `0x3b`, leaving the existing assignment list, source-order path, permutation, and all other aliases untouched.
2. Same module: the layout-name normalizer maps FFprobe's actual description `5 channels (FL+FR+LFE+BL+BR)` to `0x3b` for WAV-layout validation. It does **not** conflate this mask with FFmpeg `4.1` or `5.0`.
3. `src/convert/pipeline/dvda_realize.rs`: tests that the `WaveExtensible` validator accepts the correctly described layout and refuses stale `4.1` or `5.0` cached carrier metadata. Existing `dvda_wav_is_ready()` calls this validator on cache hits; it quarantines invalid `_wfx.wav` intermediates and rematerializes. Existing `PreserveDvdAudio` conversion and caching policy remain unchanged.
4. `src/convert/pipeline/stages.rs`: the existing source-layout test now includes assignment 18 (`0x3b` expected; `0x10f` rejected).
5. `validation/dvda_stereo_fold_corrective.py`: three new real FFmpeg/SoX decoded-sample regressions for assignment 18 (LFE-only, rear-left-only, rear-right-only), demonstrating wrong output from the old `4.1` header, correct output with `0x3b`, WAVEFORMATEXTENSIBLE mask bytes, SoX Float64 carrier roundtrip, identical second consumption, and unchanged source-order channel-preserving WAV. Existing three DVD-A regressions are retained.

No changes to stereo coefficients, effect scheduler, DC removal, certified true-peak composition, terminal quantization/dither, other DVD-A assignments, MLP reordering, or general conversion/cache machinery.

## Executed verification

- `python3 validation/dvda_stereo_fold_corrective.py` — 6/6 PASS: prior assignment 3 quad, assignment 19 center, 2.1 LFE, plus three assignment 18 regressions. For the assignment 18 defective-vs-corrected outputs (L/R):
  - LFE-only: old `0.14417/0.14417`, fixed `0.00000/0.00000`.
  - Rear-left-only: old `0.00000/0.00000`, fixed `0.18640/0.00000`.
  - Rear-right-only: old `0.10194/0.10194`, fixed `0.00000/0.18640`.
- `python3 validation/channel_layout_binding_corrective.py` — 14/14 PASS, including actual FLAC/WavPack decoded into SoX Float64 WAV.
- `python3 validation/channel_dc_smoke.py` — 13/13 PASS.
- Compiled and executed repository's independent C++ `foo_input_dvda` LPCM reference-vector generator, validated **all 18 assignment-18 vectors** against the source-to-WFX permutation for every generated decoded frame — PASS.
- FFmpeg s32le-to-WAV invocation with `-channel_layout 0x3b` before `-i`: emitted WAVEFORMATEXTENSIBLE mask `0x3b`; FFprobe returned `5 channels (FL+FR+LFE+BL+BR)`. Wrong `-channel_layout 4.1` emitted `0x10f` and FFprobe `4.1`.
- Python syntax compilation for the modified validation script — PASS.
- Patch checks and archive checksum verification — see adjacent artifact manifest.

## Qualification limits

**Not Rust-compiled/tested here:** `cargo`, `rustc`, `rustfmt`, and `rustup` are absent. Thus the new Rust tests, full workspace regression gate, real integrated DVD-A disc conversion, and release/reference qualification are outstanding. Do **not** count the earlier 7,529-test baseline as a current result. Real FFmpeg/SoX sample tests demonstrate DSP behavior but do not substitute for integration.

## Build-host acceptance

1. On a host with Rust 1.93+, run `cargo check --workspace` and `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`, plus project formatting/static/release gates as appropriate; report results without extrapolating from previous gates.
2. Run a DVD-Audio LPCM assignment 18 track with isolated LFE, rear-left, rear-right signals through the full Convert `channels: stereo` route. Confirm decoded stereo samples agree with the included reproducer, using a SoX Float64 intermediary where possible.
3. Preseed `_wfx.wav` with the old `4.1` mask and correct WFX-ordered samples. It must be rejected and regenerated as `0x3b` on the next fold, while a later request can reuse the corrected cache. Validate actual decoded samples. Verify unchanged channel-preserving materialization.
4. Re-run previous DVD-Audio assignment 3 and 19 cases, 2.1 LFE suppression, and the existing true-peak and terminal-dither compatibility tests already required by the previous handoff. Do not introduce new qualification families outside the existing contract.

## Scope judgment

This defect requires the one-table-entry role correction and matching FFprobe validation only, with focused tests. No broader hardening campaign is justified by this evidence.
