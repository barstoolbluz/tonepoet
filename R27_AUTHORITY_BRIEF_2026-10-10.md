# BRIEF — the Album Processing section, and what precision means in a human log

R26 is applied, committed and qualified. Gate **7405 passed / 1 failed** before
requalification, the failure being the expected Reference freshness one;
requalification then ran to completion — `RUNNER EXIT: 0`, every phase, 0
failures across every test-result line, both black-box smokes green, and the
real-SACD positive smoke passing with one FLAC and no manifest.

```
01c7933  Apply R26: fixture ownership ordering corrected
24ce2cd  Requalify Reference for R26: runner completes end to end
```

R26's diagnosis was better than the brief's. The R26 brief named
`generated_current_process_test_batch_id` as the discriminator, marked as a
reading to confirm or correct. The actual cause was ordering: both fixtures wrote
fragment files into the coordination workspace before claiming it, which
`ensure_conversion_log_batch_workspace_owned` refuses by design. Keeping the
generator and fixing the order was the right call, and
`nonempty_unowned_workspace_cannot_be_claimed` still passes, which is what shows
the fixtures were corrected rather than the check relaxed.

A note on how to read this. Quoted output and file citations are measured.
Everything else is our reading and may be wrong; where we say what we believe,
treat it as a starting point rather than a finding.

---

## 1. One line asks to be removed, and it was asked for in error

```
Album gain decision: submitted-batch DSD Reference music-level album gain +4.073 dB
(13 measured DSD track(s), music-level loudest true peak -4.173 dBTP;
 target -0.100 dBTP; excluded fixed Reference R64 protection/restore 12.000 dB;
 terminal applied +16.073 dB)
```

The excluded-terms clause and the raw terminal scalar should not be in a human
log. The R24 brief asked for them — *"It should state which terms it excluded,
so the figure can be reconciled against the file and against other tools"* — and
that sentence contradicted an instruction already given, that the 12 dB is
plumbing the reader does not need. R24 implemented the brief faithfully. The
request is withdrawn.

The format string is `src/convert/pipeline/stages.rs:25323`, pinned by
assertions at `:76977` and `:76978`.

**One exception worth building toward.** The rule is not that headroom is never
mentioned; it is that a step the user did not choose is not reported, and a step
they did choose is. When the custom DSD route lets someone specify their own
headroom as an explicit step, that step should appear in the log, precisely so
they can see their own instruction corroborated. That route does not exist in
Convert yet. The current 12 dB is invisible because it is ours, not theirs.

---

## 2. Precision in the human log follows what the number is

Three figures, three different answers, and the current log is inconsistent
about them — the gain decision shows three decimals while
`Applied terminal scalar` shows nine, because only the former passes through
`human_log_db` (`stages.rs:25221`, which rounds nanodecibels to 0.001).

| figure | shown as | because |
| --- | --- | --- |
| album gain applied | `+4.072834475 dB` | computed from a measurement; full precision is real information and lets the figure be checked against another meter |
| loudest true peak | full precision, subject to the check below | likewise a measurement |
| ceiling | `-0.10 dBTP` | configured; shown at the granularity the option accepts |

The principle: a measured value earns its precision, and a configured value is
shown at the granularity we accept it. Padding a two-decimal setting out to nine
invents precision; rounding a nine-decimal measurement discards it.

This narrows an earlier request. The #49 ask was for decibel values "readable at
a glance", which `human_log_db` reasonably applied to gain, peak and ceiling
alike. Only the ceiling needed it.

**Two things to check rather than assume.** We have not established that the
loudest-peak value carries nine *meaningful* digits. The gain's tail is real
because the gain is derived from the peak, but if the true-peak measurement
quantises coarser than that, printing nine digits for the peak would imply
precision the meter did not deliver. Worth confirming before the peak is
rendered at full width.

And we did not find a parse rule pinning
`--dsd-true-peak-target-dbtp` to two decimals — only the range validation
against `MIN_NORMALIZE_TARGET` / `MAX_NORMALIZE_TARGET` and the help text's
`-12.0 through 0.0`. If the parser accepts more precision than two decimals, the
parser and the log disagree and one of them needs changing; two decimals is the
intended granularity.

---

## 3. The Album Processing section reads like machine output

As it stands, for a 13-track SACD:

```
Album Processing
----------------
Uniform across all 13 successful track(s):
SACD source extraction — sacd-rs
  Source: /home/daedalus/temp/.../Surfer Rosa.iso
  Area: stereo
  Channels: 2
  Frame encoding: uncompressed DSD
  DST decoding: not required
  Tracks extracted: 13
Album gain decision: submitted-batch DSD Reference music-level album gain +4.073 dB (…)
DSD-to-PCM conversion — sox
  Target rate: 88200 Hz
  Target precision: 64-bit floating-point
Waveform gain — sox
  Applied terminal scalar: +16.072834475 dB
  Fixed Reference protection restored: +12.000 dB
Qualified Reference audio encoding — TonePoet qualified Reference path
  Sample contract: 88200 Hz, 2 channels, 24-bit signed-integer PCM, TPDF dither
  Selected by automatic policy: Resolved qualified Reference gain policy
```

Four separate problems.

**The gain appears twice with the conversion between them** — decided before,
applied after — so one subject is split across three blocks.

**`Target precision: 64-bit floating-point` invites a question it does not
answer.** A reader who asked for 24-bit will wonder whether they got it. It is
an intermediate working precision, not the output. It belongs in the structured
record.

**Two lines report our own plumbing**: `Applied terminal scalar`, which carries
the 12 dB, and `Fixed Reference protection restored`.

**Two lines are machine framing for facts that could simply be stated**:
`Sample contract:` and `Selected by automatic policy: Resolved qualified
Reference gain policy`.

**What we want**, as a shape rather than a specification:

```
Album Processing
----------------
SACD source extraction — sacd-rs
  Source: /home/daedalus/temp/.../Surfer Rosa.iso
  Area: stereo
  Channels: 2
  Frame encoding: uncompressed DSD
  DST decoding: not required
  Tracks extracted: 13

Album gain — sox
  Applied: +4.072834475 dB
  Loudest true peak: <measured value> dBTP, across 13 tracks
  Ceiling: -0.10 dBTP
  Chosen automatically, to bring the loudest peak up to the ceiling

Converted to PCM — sox, TonePoet qualified Reference path
  Output: 88.2 kHz, 24-bit integer, stereo
  Dither: TPDF
```

Read, level, write — each subject once, in the order it happened.

The gain figure above is this album's real applied scalar less the 12 dB. The
peak is left as a placeholder deliberately: the log currently rounds it to
`-4.173 dBTP`, and we have not established what the underlying measurement
holds, so we are not going to write a number here that looks measured and is
not.

On the two lines that were rewritten rather than cut:
`Selected by automatic policy: …` becomes a statement of why the number is what
it is, which is the question a reader has at that point; if a fixed gain is ever
specified instead, the same line states that from the other direction. `Sample
contract: …` becomes the output's properties on one line and the dither on its
own, because rate, depth and channel count are what the file *is* while dither
is something we chose to do to it.

`DST decoding: not required` and `Tracks extracted: 13` are kept — both carry
information — and the `Uniform across all N successful track(s)` header goes,
since the section is already album-scoped and the track count is stated.

---

## Still owed

The live Browse → Convert → preset gesture against a real SACD ISO (#43).

## Reference qualification

The tree is qualified as of `24ce2cd`. Items 1 to 3 all land in
`src/convert/pipeline/stages.rs`, which is in `REFERENCE_COMMON_SOURCE_PATHS`,
so this round will unbind it again; one requalification after all three is the
economical order.
