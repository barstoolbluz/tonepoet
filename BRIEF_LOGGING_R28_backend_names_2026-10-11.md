# BRIEF — three backend names still read as internal vocabulary

R27C is applied and committed as `bc42b52`. Gate **7411 passed / 1 failed**, the
failure being the expected Reference freshness one because `stages.rs` changed.
0 errors, 0 warnings, and it compiled first time.

```
r27c_          2 passed
r27_           6 passed
album_scope_  15 passed
r24_applied_gain_appears_once_when_uniform_and_on_each_track_when_divergent  1 passed
```

The Album Processing rewrite is visibly working in the field. A Genesis
conversion now reads `Output: 176.4 kHz, 32-bit integer, stereo`, and the
`excluded fixed Reference R64 protection/restore …; terminal applied …`
parenthetical is gone.

R27C's own correction was worth having: R27 humanized the log only when the
fragment carried `reference_album_gain`, which only the album-auto path emits,
so `--dsd-gain off` and track-scope auto would still have printed the raw
scalar, the restoration line and the 64-bit intermediate. Keying eligibility off
a completed Reference terminal is the right discriminator, and the suppression
happening in a cloned projection keeps the forensic record intact.

A note on how to read this. Quoted output and file citations are measured.
Everything else is our reading and may be wrong.

---

## Three backend names reach the log as written

From the same Genesis conversion:

```
Album gain — TonePoet Float64 scalar pump
```

Every operation line carries its backend after an em dash. Most are tool names a
reader can look up. Three are not:

| backend name | where |
| --- | --- |
| `TonePoet Float64 scalar pump` | `execution_evidence.rs`, `track_executor.rs` |
| `TonePoet Float64 scalar materializer` | `execution_evidence.rs` |
| `TonePoet DSD realization` | `execution_evidence.rs`, `stages.rs` |

Each carries a fact worth keeping. The pump one means the gain was applied by
TonePoet's own code rather than by SoX: for 32-bit integer output the gain is
applied in-process while samples stream into FFmpeg, where every other admitted
depth uses SoX's `gain` effect inside the terminal command
(`execution_evidence.rs:960`–`:966`). The Genesis log confirms the trigger —
`Output: 176.4 kHz, 32-bit integer, stereo`. So the distinction a reader cares
about is *our code or an external tool*, and that should survive. "Float64",
"scalar pump", "materializer" and "realization" are the parts that should not.

Unchanged, because they already read correctly: `sox`, `ffmpeg`, `ssrc`,
`sacd-rs`, `TonePoet file copy`, and `TonePoet qualified Reference path` — the
last of which is worth keeping precisely because it says the conversion went
through the certified route.

**What we want.** A backend is named so a reader can tell who did the work:
an external tool by its name, and TonePoet's own code as plainly as
`TonePoet file copy` already manages. The distinction between our code and an
external tool is the fact to preserve; the mechanism is not.

---

## A correction

An earlier sweep was reported as covering all internal vocabulary in the log. It
covered operation summaries only. Backend names reach the page through the same
em-dash suffix and were not examined, so this is a second category, not a
leftover of the first.

---

## A note for later, not a change now

`tonepoet::future_dsp` appears as a backend name but only inside
`#[cfg(test)]` (`execution_evidence.rs:2522`, asserted at `:2533`), so no
conversion can print it. It is a stub for the effects work, which is wanted.
When effects do land, that backend's name will appear in logs like any other, so
it will want a human name at that point rather than a module path.

---

## Still open from earlier rounds

The ceiling's granularity. R27C declined a parser clamp under explicit
non-goals, so if `--dsd-true-peak-target-dbtp` accepts more precision than two
decimals, the parser and the log disagree about what the setting is. We did not
establish which the parser does; only the range validation and the help text's
`-12.0 through 0.0` were found.

The live Browse → Convert → preset gesture against a real SACD ISO (#43).

---

## Reference qualification

Not run on `bc42b52`, deliberately. Two of the three backend names live in
source-locked files — `track_executor.rs` and `stages.rs` — so this round
unbinds the qualification regardless, and one requalification afterwards is the
economical order.
