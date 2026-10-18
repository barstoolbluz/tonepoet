# Resumable checkpoint — TonePoet channel and DC work, 2026-10-17

## Source authority

User supplied `BRIEF_channel_layout_and_dc_offset_2026-10-17.md` and a complete original `TonePoet_LOGGING_channel_dc_2026-10-17.tar.gz`. The implementation is in this **full source tree**, based directly on that upload, plus `validation/channel_dc_smoke.py`. No external repository change or Rust build has been assumed.

## State

Source changes implemented and `git diff --check` clean. FFmpeg/SoX sample-domain smoke tests pass. **Rust compile/test not executable here: cargo/rustc/rustfmt are absent.** This is not an accepted production gate. A separate Rust build-host validation and integrated media conversion are mandatory.

## Edited code paths

* `tonepoet-pipeline/src/semantic_plan.rs`: new typed `StereoFold` and `RemoveDcOffset`; explicit pan matrix and high-pass physical candidates; typed post-fold channel geometry and semantic planner tests.
* `tonepoet-pipeline/src/fingerprint.rs`: deterministic fingerprint content for these effects.
* `src/convert/pipeline/plan_bridge.rs`: typed ingress/egress channels and each lowering's output width.
* `src/convert/pipeline/stages.rs`: effect realizer input/output width propagation; certified PCM and DSD observation geometry; registered-effect execution for PCM certified channel/DC cases; DSD certified effect invocation receipts through track and album paths; existing explicit SSRC protected-replay refusal; focused regression test.
* `src/convert/pipeline/execution_evidence.rs`: completed conversion operations show matrix and DC filter.
* `src/tui/app.rs`, `src/tui/draw_output.rs`, `src/tui/convert_screen.rs`, `src/tui/button_map.rs`, `src/tui/format_interactions.rs`, `src/tui/keybindings.rs`: advanced controls, UI focus, key and mouse behavior; targeted test.
* `src/tui/command.rs`: admission and idempotent effect replacement, disable hidden DVD-A downmix for Convert requests.
* `src/tui/presets.rs`: backward-compatible save/restore and focused serialization test.
* `src/tui/analyze.rs`, `src/tui/draw_overlays.rs`: DC/AC relative classification and gain-invariance test.
* `validation/channel_dc_smoke.py`: compile-free external DSP tests derived from actual production strings.

## Independent acceptance tasks on a Rust-capable build host

1. Run `cargo fmt --all -- --check`, fix mechanical formatting, followed by `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings`. Rust compilation remains an explicit uncertainty: do **not** infer from static review.
2. Exercise the new effect path with six-channel FLAC and DVD-A ISO, SACD multichannel DSD-to-PCM (Custom), MKV/BD where fixtures exist; validate channel roles and LFE omission by decoding outputs. For 6-channel PCM+certified true peak, confirm the prepared Float64 stereo carrier is scanned with 2 channels, not the source 6. Repeat for Album scope and DSD certified Track/Album scope.
3. Confirm conversion.log includes the exact downmix and the DC high-pass **with actual physical invocation receipts** in normal, certified Track, and certified Album paths. Failure/cancellation paths should retain any completed effect receipts, including DSD album prepass failures.
4. Test the explicit protected SSRC true-peak refusal, and compare no-effect command graph/performance to the original baseline. Verify SSRC without certified true-peak behavior and the terminal quantization/dither owner remain unchanged.
5. Confirm repeated `:commit` does not duplicate intent ids/ordering edges; saved presets restore source/stereo, DC on/off, and old presets default to source/off.
6. Check pinned backend support of the exact pan/highpass commands and diff regression counts against the brief's claimed `7529/0` baseline (not verified here).
7. Investigate any compile/gate faults with **small, coherent changes only**. Do not redesign the scheduler or introduce a DSP framework for this feature.

## Known risks and bounds

* Native Rust test gate unrun. This is the principal blocker to production handoff.
* Without an authoritative speaker-role mapping for an ambiguous noncanonical multichannel source, a channel count alone cannot prove the intended mix. FFmpeg assigns a default layout to headerless Float64 carriers; test nonstandard/unspecified layouts with source fixtures and refuse where correctness is not established.
* The protected SSRC certified replay currently refuses an added post-resample channel/DC filter rather than quietly inventing a new proof. This is a deliberate, visible limitation, not a claim of complete composability with all forced resamplers.
* DSD album certified effect receipts are retained on successful prepass; the existing `String` error plumbing for DSD album prepass does not yet preserve receipts from partial executions if a later member fails. If failure-path traceability is a hard acceptance requirement, correct narrowly using the existing `CertifiedEffectPrelude` pattern from PCM rather than adding a new log format.
* The 2Hz highpass is not an exact mean-subtraction or an adaptive DC estimator. Its initial settling behavior is real and should be compared using short-duration fixtures.

## Reproduction

```bash
python3 validation/channel_dc_smoke.py
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Read `CHANNEL_DC_DELIVERY_2026-10-17.md` for the exact DSP semantics, domain restrictions, and signed-off versus unrun checks.
