# BRIEF — RF64 accepted; one dither test fails; and the metadata authority

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

## 2. Issue #76 — the authoritative metadata settings are not the authority

This is the standing item, four months old, and the one that matters most.

Saving tags on a folder of individually tagged FLAC files is refused:

```
save aborted: album-scoped COMPOSER has different values on audio files covered
by the same CUE; make the COMPOSER values match before saving; no carrier was
changed
```

The configuration asks for individual files first:

```toml
aggregate_metadata_target_priority = [
    "individual-files",
    "sidecar-cue",
    "embedded-cue",
]
```

The save is refused anyway, by a rule belonging to a carrier that was demoted.
The tags then appear written and are still there on reopening, so the claim
that no carrier was changed may also be untrue.

R33 and R33C fixed the reported instance: a one-track-per-file CUE now projects
file-scoped composer, performer, lyricist and arranger credits into
track-specific `REM` blocks instead of requiring equal COMPOSER values, and a
`SidecarCue` side admits no FLAC writes. That is not what this issue asks for.

**What we want:** the configured metadata target order is the single authority.
Nothing outside it refuses, overrides or quietly writes. A CUE beside the files
can be declared not a write target at all, and that declaration holds.

This has arrived as a run of separate edge cases across many rounds. Each fix
held and the next case appeared. The outcome wanted is the authority
established once, not this instance closed. Two observations that look relevant,
though they are ours and may be wrong:

- The gates that admit CUE work test **membership** of the configured list, not
  precedence. `metadata_album_view_dirty_unsupported_fields` and
  `cue_tonepoet_metadata_edits_for_album_sidecars`, both in
  `src/tui/keybindings.rs`, ask only whether `SidecarCue` appears anywhere in
  it.
- A target cannot be omitted either.
  `normalized_aggregate_metadata_target_priority` in `src/config.rs` chains the
  defaults onto the configured order, so anything left out is appended back.

Together those mean no value of the setting can express "do not write the CUE".

A refusal that says no carrier was changed should also be true, or should say
which carrier it means.

The full entry is `## 76` in `docs/OUTSTANDING_ISSUES.md`.

## Not in this round

Three things need the user at a terminal, not a delivery: the five R5 UI
changes have never been looked at; #75's SACD tag-transfer round trip has never
run against a real disc; and the dither symptom above needs the in-field toggle
sequence with `dither_overridden`, `dither_override_from_preset`,
`deemphasis_enabled` and the source bit-depth facts inspected, as the R20
handoff asks.

Issue #73, markdown in the popup body, is deliberately not in this round.
