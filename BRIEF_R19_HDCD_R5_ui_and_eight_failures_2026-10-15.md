# BRIEF — R4D: the migration defect is fixed, eight tests fail, and five UI changes

R4/R4C/R4D applied. It compiled as delivered. The v28 migration defect is
resolved: `db::tests` is 83 of 83, and the whole upgrade matrix passes.
`r4d_` 3 of 3, `r4c_` 6 of 6.

Gate: **7490 passed / 9 failed**. One is the expected source-lock freshness
failure — `plan_bridge.rs`, `plugins.rs` and `semantic_plan.rs` moved, so
requalification is owed. The other eight each fail 1 of 1 alone.

## 1. Two new AAC tests die on their own fixture's output path

```
plugins::tests::libfdk_aac_quantizes_a_wide_carrier_once_at_its_terminal_boundary
plugins::tests::certified_true_peak_lossy_aac_does_not_gain_an_unqualified_dither_boundary

InvalidSettings { field: "output_path", reason: "AAC output must use an .m4a,
.m4b, or .mp4 container extension unless an explicit raw-AAC mode is
implemented" }
```

Both are new in this round. They never reach the terminal-boundary and
dither-boundary behaviour they were written to check.

## 2. A certified DSD route changed its arguments — pre-existing test

```
plugins::tests::ffmpeg_lossy_album_gain_pins_measured_encoder_input_rate
certified DSD carrier must retain its previously qualified gain/rate path:
  -af volume=2.125000000dB:precision=double,
      aresample=resampler=soxr:precision=33:cutoff=0.950:
      dither_method=triangular:out_sample_fmt=s16
  -ar 96000 -sample_fmt s16 -c:a libfdk_aac
```

The assertion is that a certified DSD carrier's command contains no
`aresample=` at all. It now contains one, carrying a soxr resample, triangular
dither and an s16 conversion.

## 3. Lossy targets lost their direct route

```
convert::pipeline::stages::…::cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg
src/convert/pipeline/stages.rs:68880
  Auto Aac must remain directly streamable
```

## 4. The new HDCD policy test cannot set the depth it tests

```
tui::app::hdcd_format_policy_tests::hdcd_defaults_follow_resolved_precision_and_lossy_codec
src/tui/app.rs:25102
  assertion failed: state.bit_depth.select_value(&depth)
```

Our reading: the failing line is `:25102`, inside the float loop. The test selects
`AudioFormat::Wav` at `:25100`, then selects `Float32` at `:25102`, and only
calls `apply_format_constraints()` at `:25103`. The pill is still carrying the
previous format's constraints, which disable float, so the selection is
refused. This is the round's own test for the revised defaults.

## 5. Three failures also present in the previous gate, still failing

The first was described in the last brief as part of the migration cluster; the
other two were in that gate's output but were not written up.

```
tui::app::app_startup_options_tests::quiescent_v23_startup_activates_v24_protocol_then_current_schema_without_fallback
tui::command::execute_queue_state_consistency_tests::async_browse_queue_review_commit_preserves_cue_warning_after_final_commit_status
tui::context_menu::tests::context_menu_convert_last_used_audio_directory_commits_after_async_expansion
```

---

# UI changes

## A. Remove the warning emoji

`src/tui/app.rs:7082` renders
`"⚠ HDCD Decode Off: conversion may destroy HDCD control codes."`

**What we want:** no emoji. The warning stays; the glyph goes.

## B. Hide the HDCD row when nothing is actionable

The row currently surfaces for Transient-Filter-only and passive HDCD.

**What we want:** the row appears only when decoding would do something.
Decoding those sources gets the user nothing, so the control is noise. This
supersedes the original R19 brief, which asked for presence to be signalled
above the fold in those cases. The Transient-Filter-only and passive help
messages become unreachable and should go with it.

## C. Drop the track counts from the row suffix

The row reads `HDCD Decode   off    on   ⓘ HDCD: 6 of 6 tracks (6 decode-capable)`.

**What we want:** no counts. TonePoet handles per-track selection itself; the
user only needs to know the material is present.

## D. Put the true-peak information glyph where the other two are

De-emphasis and HDCD register their glyph at `label_col + 13`, after their
pills. True peak registers at `area.x + true_peak_info_offset()`, between the
label and the value: `TP target ⓘ  < -0.10 dBTP >   custom`.

**What we want:** the true-peak glyph at the end of the row, consistent with
the other two.

## E. Replace the HDCD help text

The current text names control codes, Peak Extend, Low-Level Gain Adjustment,
Transient Filter and FFmpeg. Replace it with this, verbatim:

> Some CDs were mastered with HDCD, which encodes additional dynamic-range
> information in ordinary 16-bit audio. The disc plays normally without an HDCD
> decoder, but decoding can restore dynamics in loud and quiet passages.
>
> HDCD decoding recovers up to 20-bit precision. For lossless output, TonePoet
> automatically selects 24-bit to preserve the recovered precision. If you
> choose 16-bit instead, TonePoet applies TPDF dither by default. You can turn
> it off.
>
> On — TonePoet decodes supported HDCD features before other processing,
> restoring the available dynamics. Recommended for listening copies and lossy
> conversion, because lossy encoding destroys the embedded HDCD information.
>
> Off — TonePoet leaves HDCD decoding disabled. A lossless copy that preserves
> the original 16-bit, 44.1 kHz samples without modification also preserves the
> HDCD information for later decoding. Resampling, dithering, other
> sample-altering processing, or lossy encoding can destroy that information.
> Simply increasing bit depth without altering the samples does not.

The popup renders one uniform style with no markup, so On and Off use the
em-dash form the pre-emphasis message uses rather than bold. Issue #73 covers
markdown rendering and is not asked for here.

## Requalification

Owed. `plan_bridge.rs`, `plugins.rs` and `semantic_plan.rs` moved.
