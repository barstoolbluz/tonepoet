# BRIEF — a single-file album's log never refreshes; plus a label, a recurrence, and two issues to close

R22C is applied, committed and requalified. One defect from it, found by the
delivery's own smoke; then four items folded in from the outstanding list.

```
05b99b0  Apply R22C: log history only on displacement, human-readable encode block, ISO preset defers to probe facts
b8b9e74  Requalify Reference for R22C; runner then caught a real single-file log defect
```

Gate before requalification: **7386 passed / 1 failed**, the single failure being
`installed_reference_qualification_binds_the_current_locked_sources`, expected
because `stages.rs` is source-locked. All five new `r22_` tests pass. 0 errors,
0 warnings.

R22C did not compile as delivered: three sites in the new `command.rs` tests
reached `SourceMetadata` through a private import (E0603). Fixed locally to the
path already used elsewhere in that file.

Requalification ran and installed cleanly —

```
Generated report, certification, evidence and source lock are coherent.
Reference qualification installed and freshness gate passed.
Issue #67 CLI exit smoke: all cases passed.
```

— with 0 failures across every test-result line, then exited 1 at item 1 below.

---

## 1. A single-file album's conversion log is never regenerated

The delivery's own smoke caught this:

```
R20 album publication smoke FAILED: N=1: second run must archive exactly the displaced report (0)
```

The smoke is right, and the cause is upstream of the archiving. For a
**single-file** album the log is rewritten on every run but never regenerated.
Three consecutive conversions into the same destination, measured directly:

```
N=1:  Generated (UTC)  23:33:01 / 23:33:01 / 23:33:01     archives 0 / 0 / 0
      file mtime            :01 /      :04 /      :08
N=3:  Generated (UTC)  23:33:11 / 23:33:15 / 23:33:19     archives 0 / 1 / 2
```

The file is touched each run — mtime advances — while its `Generated (UTC)`
header stays frozen at the first run. After the third conversion
`conversion.log` still describes the first. No archive is created because
nothing ever changes to archive, so the archive logic is behaving correctly on
top of a report that was never refreshed.

The multi-file path is correct, and #70's corrected specification is met there:
a greenfield run leaves only `conversion.log`, a second leaves the new log plus
one dated archive, and at N=2 that archive's SHA-256 equals the first run's log
exactly.

**What we want.** Every conversion that writes a log writes a log describing
*that* conversion, whatever the album's size. A single-file album is then
subject to the same history rule as any other: the displaced report is preserved
when it is replaced.

---

## 2. `partial (source)` does not say what it does

The Output Options pill currently reads:

```
partial (source)    off    on
```

"(source)" was meant to signal that this governs tracks within one multi-track
source rather than separate files, which have needed no flag since #68. It does
not convey that, and "partial" alone does not say what happens either way.

Its sibling pill states a condition and its outcomes — `if exists`, with `fail`,
`overwrite` and `keep both`.

**What we want.** A label that says what the setting does and what happens on
either side of it, in the same shape as its sibling. The scope matters and is
currently lost: this governs tracks inside one album, not separate files. The
CLI flag stays `--partial`.

---

## 3. Issue #43 has recurred, and the gap is how it was closed

The ISO preset refusal corrected in R22C is not a new defect. It is #43, filed
2026-09-24, whose status reads:

```
**Status 2026-09-26:** fixed on main @ 6d11d35 (v0.5.3), regressions green; live TUI check on the
Dark Side ISO still owed by the user. The preset is deferred until the source probe resolves;
inapplicable DSD fields are refused, not skipped; saved PCM presets carry no DSD fields; Int32
dither is limited to what the planner admits.
```

The live TUI check was never performed. When it finally was — this week, on a
different SACD ISO, through the same Browse context-menu gesture — the same six
fields were refused. The v0.5.3 fix deferred "until the source probe resolves",
and an ISO slipped through because its placeholder source mode does not claim a
probe is in progress. R22C addresses exactly that hole, though the gesture
itself has not been re-run here — see below.

Issue #46 is the other half of the lesson. It carries the identical six-field
message from the CLI and was resolved by treating the other source class's
fields as dormant (`src/tui/presets.rs:1154`). That tolerant behaviour was never
given to the Browse path, which is why the same six fields still produced a
fatal refusal there two releases later.

**What we want.** #43 closed as a recurrence rather than as a new finding, and
not marked fixed until the Browse-context-menu gesture has actually been
performed against an SACD ISO. A regression test exists now
(`r22_folder_iso_reference_preset_applies_only_after_dsd64_probe_facts`); the
field check is still the thing that was missing both times.

---

## 4. Issue #49 is substantially fixed, with one residual

#49 recorded the conversion log burying essential facts. Measured on two current
field logs:

| | 10,000 Maniacs | Duke Jordan (SACD) |
| --- | --- | --- |
| lines | 296 | 182 |
| longest line | 153 | 168 |
| staging-path lines | 0 | 0 |
| `Source ref` lines | 0 | 0 |

Against #49's own figures — 292 lines, a 702-character longest line, 48
staging-path-only lines, and true peak and gain unlabelled inside a `Source ref`
line — those complaints are gone. The gain decision is now a labelled sentence.

The residual is numeric precision. The same line reads:

```
Album gain decision: submitted-batch DSD Reference album gain +19.028069515 dB
(8 measured DSD track(s), loudest true peak -19.128079335 dBTP; target -0.100000000 dBTP)
```

Three such values per Reference log, none in the PCM log: `+19.028069515 dB`,
`-19.128079335 dBTP`, `-0.100000000 dBTP`.

The precision is deliberate in the data and accidental in the presentation. The
value is carried as a nano-decibel type and rendered straight out, for example
`format!("{dbtp:.9}")` at `src/convert/pipeline/stages.rs:31000`. So this is a
formatting change at the human-log boundary, not a change to what is measured or
stored.

**What we want.** Decibel values in the human log readable at a glance. Full
precision stays in the data and in the structured record, where the exact values
are needed and where no one reads them by eye. With that done, #49 can close.

---

## 5. Issue #71: the strict inventory never runs against overwrite or keep-both

This rides along because it touches the same tests and the same smoke as item 1.

Four strict destination-inventory tests publish and then assert the
destination's exact file set. Every one pins `OverwritePolicy::FailIfExists`, so
none ever publishes over an existing album:

```
issue_65_postconversion_destination_inventory_covers_source_routes_and_log_consent   FailIfExists=1 ReplaceWithBackup=0 keep-both=0
issue_65_independent_folder_album_publish_has_no_extra_entries_with_log_on_or_off    FailIfExists=0 ReplaceWithBackup=0 keep-both=0
issue_65_reference_execution_evidence_does_not_force_unrequested_manifest            FailIfExists=1 ReplaceWithBackup=0 keep-both=0
issue_65_reference_execution_still_validates_sample_identity_without_manifest        FailIfExists=2 ReplaceWithBackup=0 keep-both=0
```

That is the path #69 broke on, where an album lost two of three tracks while the
gate stayed silent. The black-box smoke does exercise overwrite and keep-both,
but it counts audio files and looks for backup directories by name; the strict
inventory enumerates every entry and fails on anything unexpected, which is how
the `.tonepoet-batch` leftover and the snapshot log were both caught.

**What we want.** The strict inventory extends to `--if-exists overwrite` and
`--if-exists keep-both`, asserting the complete file set afterwards, including
the sibling directory keep-both creates.

---

## Reference qualification

Items 1, 4 and 5 all land in `src/convert/pipeline/stages.rs` — the log
generation, the decibel rendering (`:31000` and its neighbours), and the strict
inventory tests, which live in that file. It is in
`REFERENCE_COMMON_SOURCE_PATHS`, so this round unbinds the installed
qualification again. `scripts/requalify_reference_r20.sh` is unchanged and has
worked on the last three rounds.

Item 2 is in the TUI and item 5's smoke is a shell script; neither is locked.
