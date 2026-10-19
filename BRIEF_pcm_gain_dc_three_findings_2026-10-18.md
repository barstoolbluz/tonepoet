# BRIEF — two failing tests and a misplaced information glyph

Applied, compiled as received, 1129 of 1129 checksums. Gate: **7572 passed /
3 failed**, zero aborted binaries.

One failure is the expected source-lock freshness; all eight locked files
moved. Requalification is held until the other two are fixed, since the tree
changes again.

Both findings from the independent review read right to us, though we did not
verify the planner reasoning ourselves. The prerequisite you found yourselves — the native DC effect declared as an unspecified transform,
so the strong Binary64 SSRC gate rejected it — was the correct thing to
correct in the contract rather than in the worker.

## 1. The new constraint leaks into a path it should not touch

```
semantic_plan::tests::multichannel_dc_constraint_does_not_perturb_stereo_fold_or_legacy_paths
  left  Some(Ffmpeg)
  right Some(Sox)
```

New this round, 1 of 1 alone. The test asserts a stereo-fold plus manual-DC
route keeps selecting Sox and does not acquire the multichannel raw-layout
contract. It now selects FFmpeg. Your own guard against the new constraint
reaching unrelated paths is reporting that it reaches them.

**What we want:** the stereo and legacy routes select what they selected
before, and the multichannel raw-layout obligation attaches only where
multichannel native DC genuinely requires it.

## 2. The DC notice does not scroll where the test requires it

```
tui::convert_screen::format_render_registration_tests::dc_info_hitbox_four_pills_and_readable_notice_at_terminal_widths
  assertion failed: notice_message_max_scroll(...) > 0
```

New this round, 1 of 1 alone.

**What we want:** the readability property the test states holds, or the test
states the property that is actually wanted.

## 3. The PCM gain information glyph is in the wrong place

The PCM gain row splices the glyph into the label span, so it renders between
the label and the first pill:

```
PCM gain ⓘ   off   true-peak-guard   true-peak-normalize   fixed-gain
```

Every other information control puts it after the pills. De-emphasis and HDCD
register past theirs; DC passes it as the row suffix, which renders after the
pills; and true peak computes an offset two cells past its last pill. PCM gain
is the only one that does not.

The splice also engineers pill geometry back into place, which the comment
acknowledges.

**What we want:** the PCM gain glyph at the end of the row, consistent with
every other information control, without the label span being rewritten to
accommodate it.
