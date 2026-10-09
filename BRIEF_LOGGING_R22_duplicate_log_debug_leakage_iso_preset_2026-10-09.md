# BRIEF — a duplicate log I specified wrongly, raw Debug output in the log, and an ISO preset refusal

R21 is accepted and qualified; `origin/main` is `c08886d`. Three items follow,
all found in the field on that build. The first is a correction to the R20 brief
rather than a defect in the delivery.

---

## 1. The dated log should exist only when it is preserving something (#70)

**This was mis-specified in the R20 brief, not mis-built.** That brief said:

> **What we want.** A per-run log named with its date and time, written alongside
> the existing `conversion.log`, so history accumulates while the familiar
> filename stays where it is.

"A per-run log" is wrong, and R20 implemented it faithfully. The result is that a
greenfield conversion — one into a folder with no prior log — writes two files
whose bytes are identical. Measured on three fresh field conversions:

```
Duke Jordan - Flight To Jordan (2010) [FLAC]
  conversion-20261009T190432.062253035Z-8a9db71204a56f8a.log   7655 bytes
  conversion.log                                               7655 bytes
  distinct sha256 across both: 1

Herbie Hancock - Sextant (2019) [FLAC]
  conversion-20261009T201157.978795000Z-3162e2405d3c0dd3.log   6077 bytes
  conversion.log                                               6077 bytes
  distinct sha256 across both: 1
```

The 10,000 Maniacs conversion is the same shape at 12047 bytes each. Two of these
are SACD sources and one is a FLAC folder album, so the duplication is not
route-specific.

The original purpose was narrower. It was established that `conversion.log` is
rewritten by the next conversion into the same folder, and the point was to stop
losing the earlier one. A folder with no prior log has nothing to preserve.

**What we want.** A dated copy is written only when an existing `conversion.log`
is about to be replaced, and it holds the log being displaced. A conversion into
a folder with no prior log writes `conversion.log` and nothing else. The
timestamp belongs to the run whose log the file contains, not to the run that
displaced it, and still needs enough resolution that two conversions on the same
day cannot collide.

So a first conversion leaves one file; a second leaves the new `conversion.log`
plus one dated file holding the first run. No duplication at any point.

Consequence for the tests: the #65 strict destination-inventory tests were
extended in R21 to expect exactly one dated snapshot on a first human-log run.
That expectation now inverts — a first logged run must have **no** dated log, and
the dated file appears only on a second run into the same destination.

---

## 2. The log prints raw Rust `Debug` output where it should print plain language

From the 10,000 Maniacs conversion, a 96 kHz/24-bit FLAC source encoded to
24-bit FLAC:

```
  Processing:
    PCM encoding / terminal realization — ffmpeg
      Format: Flac
      Target precision: Int24
      Terminal processing: false
      Terminal realization: FfmpegDirect
      Terminal input precision: Int24
      Terminal input value domain: IntegerLattice(Int24)
      Terminal output format: Flac
      Terminal output precision: Int24
      Terminal sample/quantization owner: FFmpeg
      Dither owner: none
      Effective terminal dither: None
```

`FfmpegDirect` and `IntegerLattice(Int24)` are Rust enum variants reaching the
page through `{:?}`:

```
src/convert/pipeline/execution_evidence.rs:1655
    EvidenceValue::Text(format!("{:?}", realization.kind)),
src/convert/pipeline/execution_evidence.rs:1666
    EvidenceValue::Text(format!("{:?}", realization.input_value_domain)),
src/convert/pipeline/execution_evidence.rs:1670
    EvidenceValue::Text(format!("{:?}", realization.target_format)),
src/convert/pipeline/execution_evidence.rs:1674
    EvidenceValue::BitDepth(format!("{:?}", realization.target_bit_depth)),
```

That file has 49 `Debug` renderings in all — 11 written as `format!("{:?}", x)`
and 38 as `format!("{x:?}")` — so this is a pattern rather than four lines. It is also the same defect class as the `{sample_contract:?}` dump
found and removed earlier in this series: an internal type spelling used as
human-facing text.

"Terminal" is internal vocabulary for the final stage of the pipeline. It is not
a word for a user, and nine of those ten lines begin with it.

**What we want.** The same facts, in language a listener can read. Those ten
lines say one thing — FFmpeg encoded the audio straight to 24-bit FLAC, and
nothing was resampled, requantised or dithered. A shape that carries every fact
the block currently carries:

```
  Encoding:
    Encoder:           FFmpeg (direct encode, no sample processing)
    Input:             24-bit integer, 96 kHz
    Output:            FLAC, 24-bit
    Bit-depth change:  none
    Dither:            none (not needed — bit depth unchanged)
```

and when work is actually done, the same shape reports who did it:

```
  Encoding:
    Encoder:           SoX → FFmpeg
    Input:             24-bit integer, 96 kHz
    Output:            FLAC, 16-bit
    Bit-depth change:  24-bit → 16-bit, by SoX
    Dither:            triangular, by SoX
```

Nothing is lost: quantisation ownership, dither ownership, input and output
precision, and the value domain all survive as plain statements. `Int24` becomes
`24-bit`, `IntegerLattice` becomes `integer`, and `Terminal processing: false`
becomes the parenthetical on the encoder line, since what it reports is that the
final stage passed samples through untouched.

The wording above is a recommendation, not a specification. What matters is that
no line in a human log is produced by `{:?}`, and that no user-facing line needs
the word "terminal".

---

## 3. A DSD preset is refused when an ISO is converted from the Browse screen

Reproduction, on `c08886d`: in Browse, select the folder containing an SACD ISO,
right-click, choose Convert, then choose a user preset for SACD/DSD64 to PCM
FLAC. The Convert screen opens and the status bar shows:

```
preset 'SACD-or-DSD64-to-PCM-FLAC' refused fields: dsd_path, dsd_profile,
dsd_gain, dsd_true_peak_target_dbtp, dsd_true_peak_scope, dsd_true_peak_scan
```

The same preset applies correctly when loaded by hand in the Convert screen.

The six fields are refused together because they share one gate:

```
src/tui/app.rs:5549
    self.source_is_dsd && !self.is_dsd_selected()
```

When that is false, every `dsd_*` field present in the preset is refused in one
pass (`src/tui/presets.rs:616`), by design — a field in a preset is deliberately
never silently dropped.

For a source whose identity is not probe-proven, `source_is_dsd` comes from a
hint, and the hint is decided by file extension alone:

```
src/tui/app.rs:454
    .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "dsf" | "dff"))
```

`.iso` is not in that set, so an SACD ISO is hinted as not-DSD. After a completed
probe the value instead comes from real source facts
(`src/tui/app.rs:7960`), which is why the manual Convert-screen path works.

Two further observations, offered as observations:

- The two preset entry points disagree about severity. `load_queue_preset_into_pills`
  (`src/tui/command.rs:8798`) treats any refusal as fatal and rolls the preset
  back — it emits the message above. The other path
  (`src/tui/presets.rs:1154`) filters refusals whose names carry a dormant
  prefix, tolerating `dsd_*` refusals when the source is not DSD.
- Machinery for this case already exists and did not prevent it. The guard at
  `src/tui/command.rs:9382` defers preset application until the probe lands, and
  its comment names this exact scenario: *"essential for generic ISO/SACD
  sources: the path alone cannot establish DSD availability, so applying now
  would interpret the preset against the previous/placeholder source and either
  drop or refuse valid DSD fields."* Either the deferral does not fire for a
  folder-expanded selection, or the deferred application at
  `src/tui/command.rs:9433` still runs while the identity is a
  not-DSD extension hint. Which of the two has not been established here.

**What we want.** A preset that selects the qualified Reference DSD-to-PCM route
applies to an SACD ISO chosen from the Browse screen, exactly as it does when
loaded by hand in the Convert screen. Where a source's DSD identity cannot be
known from its path, the preset waits for that identity rather than being
interpreted against a guess and refused.

---

## Reference qualification

The dated-log naming is written in `src/convert/pipeline/stages.rs`
(`:18740`, `:18822`, `:18824`), which is in `REFERENCE_COMMON_SOURCE_PATHS`, so
item 1 will unbind the installed qualification. `scripts/requalify_reference_r20.sh`
ran cleanly on the last two rounds and can be reused unchanged; a handoff in the
same form would be useful.

Not locked, for planning: `src/convert/pipeline/execution_evidence.rs` (item 2),
and `src/tui/app.rs`, `src/tui/command.rs`, `src/tui/presets.rs`,
`src/convert/source_admission.rs` (item 3).
