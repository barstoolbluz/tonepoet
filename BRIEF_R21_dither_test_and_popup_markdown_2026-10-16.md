# BRIEF — RF64 accepted; one dither test fails; and popup markdown

R20/R20C applied. Gate: **7512 passed / 1 failed**. No source-locked file
moved, so the installed Reference qualification still binds.

RF64 is accepted: `rf64_` is 23 of 23. The retry reaches Convert's
pre-emphasis evidence, both advisory entrypoints, Browse's tag reads and sort
keys, and track-number sorting, and a successful empty read stays distinct
from a failed one.

It did not compile as received: `src/tui/rf64_metadata.rs:128` chained
`guess_file_type()` into `probe.read()` through `and_then`, but those fail with
`io::Error` and `LoftyError`. Split here into two steps with the same error
text. Ninth delivery that did not compile.

## 1. The new de-emphasis dither test fails

```
tui::app::hdcd_format_policy_tests::hdcd_off_preserves_user_dither_and_independent_deemphasis_tpdf
src/tui/app.rs:25235
  HDCD Off must not remove independently automatic CD de-emphasis TPDF
  left  None
  right TPDF
```

New this round, 2 of 2 alone. The other assertions in it pass, including every
explicit-dither case.

Declining to change production state without a reproduction was the right
call, and the field observation stands unexplained. But this test says HDCD Off
removes a TPDF that CD de-emphasis independently requires, which is adjacent to
the reported symptom rather than separate from it.

We could not see why it fails. `apply_auto_dither` ORs the de-emphasis and HDCD
predicates, and in that fixture the de-emphasis predicate's conditions all look
satisfied — `deemphasis_enabled`, FLAC is lossless PCM, depth is Int16. The
production dither logic did not change this round, so whatever this is, it is
existing behaviour.

**What we want:** either the test establishes the precondition it depends on
and passes, or the hole it found is closed. Which of those it is, is the
question.

## 2. Issue #73 — popup body text renders no emphasis

`draw_scrollable_message_popup` renders one uniform style with `Wrap { trim:
true }`, so markdown appears literally and an indented continuation loses its
indent. Three long-form help messages now exist — `DEEMPHASIS_INFO_MESSAGE`,
`HDCD_INFO_MESSAGE`, `TRUE_PEAK_INFO_MESSAGE` — and the HDCD text was authored
with bold that had to be stripped before it could ship.

**What we want:** the popup renders a markdown subset, styled for the terminal.
Links, tables and images are not wanted.

Two constraints. Existing plain-text messages must look unchanged, so stray
punctuation must not become emphasis. And `wrapped_row_count` bounds scrolling
in sixteen places while counting rows from raw text by whitespace wrapping; if
layout strips markers or indents continuations, the count and the drawn body
have to come from the same place or scrolling stops matching the content.

No markdown crate is in the workspace. The full entry is `## 73` in
`docs/OUTSTANDING_ISSUES.md`.

## Not in this round

Three things need the user at a terminal, not a delivery: the five R5 UI
changes have never been looked at; #75's SACD tag-transfer round trip has never
run against a real disc; and the dither symptom above needs the in-field toggle
sequence with `dither_overridden`, `dither_override_from_preset`,
`deemphasis_enabled` and the source bit-depth facts inspected, as the R20
handoff asks.

#76 remains open and is the largest outstanding item — the configured metadata
target order is still not the single authority it asks to be. It is not folded
in here because it deserves its own round rather than a partial pass.
