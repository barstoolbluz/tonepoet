# BRIEF — R20 is accepted; the fixtures need bringing up to the new behaviour

R20 is right. All four asks are implemented and verified on real audio, Reference
is requalified, and the eight remaining failures are all test fixtures still
describing the old behaviour. Two of them carry a decision rather than a
re-baseline; both are marked below.

`origin/main` after this round:

```
075946c  Apply R20C2: #69/#70/#68 implemented and verified on real audio; 7 fixture/behaviour failures to resolve
9b048df  Requalify Reference for R20, via the delivered runner
ea662d8  Record the 400 ms ReplayGain fixture floor; it cost a round of misdiagnosis
```

The delivery arrived as the R20C2 corrective bundle; R20 and R20C were never
delivered separately, so the full change set was applied from the complete source
archive. All six delivery checksums verified, the Git bundle self-reported a
complete history, and the standalone smoke script was byte-identical to the
tarball's copy. Both new scripts arrived mode 775 and were normalized to 755 to
match `scripts/requalify_reference_r18.sh`.

## What is verified

Gate, after requalification: **7376 passed / 7 failed**. No compile errors, two
warnings (see fixture 6). All six new R20 tests pass.

Requalification, via the delivered `scripts/requalify_reference_r20.sh` with a
real SACD ISO:

```
qualification: test result: ok. 1 passed; 0 failed; ... finished in 896.26s
coherence:     Generated report, certification, evidence and source lock are coherent.
install:       Reference qualification installed and freshness gate passed.
freshness:     test result: ok. 3 passed; 0 failed; ... finished in 0.17s
```

The runner then exited 101 at the `issue_65_reference_execution_evidence…`
fixture below. That happened after installation and the freshness gate, and the
runner clears `install_started` at that point, so the sidecars were correctly not
rolled back. The runner never reached its two black-box smokes; they were run
separately.

Behaviour re-verified twice, with fresh sources the second time:

```
#69 overwrite: album flacs=3   backup dirs=0   dotfiles=0   exit=0
#70 snapshots: 2 snapshot logs + 1 conversion.log after 2 runs
keep-both:     Recheck -> 3 flacs ; "Recheck (2)" -> 3 flacs
#68 survivor:  REPLAYGAIN_ALBUM_GAIN=3.91 dB ; Contributing files: 1 ; Excluded files: 1 ; exit=1
manifests created anywhere: 0
```

`scripts/smoke_cli_convert_exit_codes.sh` passes every case, including the one
that could not pass before R20 fixed #68:

```
PASS positive exit 0 (1/1 succeeded, 0 failed)
PASS negative exit 1 (0/1 succeeded, 1 failed)
PASS mixed    exit 1 (1/2 succeeded, 1 failed)
```

`scripts/smoke_r20_album_publication.sh` passes everything except its last
assertion:

```
PASS overwrite N=1: all audio survives, no backup remains, history retained
PASS overwrite N=2: all audio survives, no backup remains, history retained
PASS overwrite N=3: all audio survives, no backup remains, history retained
PASS unlogged single-file overwrite: both unselected tracks and log history survive
PASS keep-both: one numbered sibling contains all three tracks
R20 album publication smoke FAILED: surviving FLAC lacks ReplayGain album tags
```

## The fixtures

Six items. The first five account for all eight failures — seven in the gate, one
in the smoke — in this arrangement: #65 inventory (1), smoke ReplayGain (1),
field cycle (2), Actions row (3), prior manifest (1). The sixth is the two
warnings, which fail nothing but were asked for last round.

### 1. The third strict #65 inventory test was not extended

```
issue_65_reference_execution_evidence_does_not_force_unrequested_manifest
  unexpected entries: ["Gate Test/conversion-20261009T145325.919186079Z-16eb575ba2f34b32.log"]
```

The delivery states both strict destination-inventory tests were extended to
expect one timestamped snapshot. There are three; the Reference one still refuses
the snapshot log. The behaviour is correct and intended.

### 2. The smoke's ReplayGain fixture is a quarter of a second long

This one looked like a ReplayGain defect and is not. `make_wavs()` writes mono
11025-frame WAVs, which is 0.250 s. Integrated loudness is measured in 400 ms
gating blocks, so audio shorter than one block has no computable gain: TonePoet
writes `REPLAYGAIN_*_PEAK` and omits `REPLAYGAIN_*_GAIN`. The surviving file in
the smoke's own run carried only the peaks.

Measured with a mono 44.1 kHz tone, everything else held constant:

| frames | duration | album gain |
| --- | --- | --- |
| 11025 | 0.250 s | missing |
| 17640 | 0.400 s | 8.15 dB |
| 22050 | 0.500 s | 8.15 dB |
| 44100 | 1.000 s | 8.15 dB |
| 88200 | 2.000 s | 8.15 dB |

Reproducing the fixture's exact invocation with audio over 400 ms gives
`REPLAYGAIN_ALBUM_GAIN` as expected. The fixture needs audio of at least one
gating block; the product is behaving correctly.

### 3. Two Output Options field-cycle tests

```
output_options_field_cycle_includes_companion_fields_when_maximized   (src/tui/app.rs:7815)
output_options_field_cycle_matches_rows_rendered_for_small_maximized_panes  (:7837)
  left: Partial   right: Actions
```

The cycle now yields the new `Partial` field where `Actions` was expected.

### 4. Three Actions-row render tests — and a decision

```
maximized_actions_row_registers_button_map_target_inside_pane_only  (src/tui/draw_output_options.rs:950)
maximized_actions_row_renders_live_pipeline_summary                 (:931)
wrapped_output_options_draw_populates_button_map_for_actions_row    (:1000)
  left: None   "the production Output Options draw path must register the rendered Actions row"
```

R20 moved the row and raised the visibility threshold to match:

```
before: if show_actions && maximized && area.height >= 20 ...   ACTIONS_ROW = 18
now:    if show_actions && maximized && area.height >= 22 ...   ACTIONS_ROW = 20
new:    PARTIAL_ROW = 16, IF_EXISTS_ROW = 17
```

That is coherent — two more rows are occupied, so the pane needs to be two rows
taller. The three tests pin the old threshold by rendering into panes of height
20, which is why they now register nothing.

**The decision.** A maximized Output Options pane of height 20 or 21 no longer
shows the Actions row at all, where it did before. We want the Actions row to
remain reachable at the pane heights it used to work at, rather than the two new
pills costing it. How that is achieved is open — the two new rows could be placed
differently, the rows could compact, or the pane could scroll. If instead the
right answer is that Actions legitimately needs a taller pane, then the tests
should be re-based on 22 and the trade-off is accepted knowingly.

### 5. The prior-manifest expectation, resolved by decision

```
matching_prior_manifest_does_not_suppress_fresh_publish_with_backup
  tests/chunk_2_1_2_manifest_publication.rs:214
  "a new manifest requires opt-in; the old one belongs to the backup"
```

Reproduced with the release binary: plant a legacy `.tonepoet-manifest.json` in
an existing album, re-convert with `--if-exists overwrite`, and it is still in
the album directory afterwards. The old whole-album replace carried it into the
backup; R20's incremental publish only touches files it produced, so it leaves
it.

**The decision is that this is fine.** The user's words: *"PRE-existing? not
created by the run? that's fine."* A conversion that creates no manifest is the
property that matters, and that is verified — no manifest is created on a clean
convert, on an overwrite, or on keep-both. So the test's expectation is what
should change: assert that no *new* manifest is written and that
`published.manifest_path` is none, rather than that no manifest is present.

### 6. The unused imports are still there

R20 did not clear these, and the R20 brief asked for them. They are the two
warnings in this build:

```
warning: unused import: `SsrcPdfType`
   --> src/convert/pipeline/stages.rs:124:82

warning: unused imports: `ConversionManifestTrack`, `ConversionManifest`,
         `ManifestTrackExecutionIdentityV2`, `TrackIdentity`,
         `metadata_mtime_secs`, and `write_manifest`
     --> src/convert/pipeline/stages.rs:65942:24
```

The second set was left by R18's rerun withdrawal; the line has only moved, from
`:65651` to `:65942`, as R20 added code above it. `SsrcPdfType` predates both.

## This fixture round needs its own requalification

Worth knowing before the work is planned: the `issue_65_reference_execution_…`
fixture is an inline test module inside `src/convert/pipeline/stages.rs`, which
is in `REFERENCE_COMMON_SOURCE_PATHS`. So does the import cleanup above. Editing
either unbinds the installed qualification again, exactly as R20 did.

The other fixtures are not locked:

| file | locked |
| --- | --- |
| `src/convert/pipeline/stages.rs` | yes |
| `src/tui/app.rs` | no |
| `src/tui/draw_output_options.rs` | no |
| `tests/chunk_2_1_2_manifest_publication.rs` | no |
| `scripts/smoke_r20_album_publication.sh` | no |

`scripts/requalify_reference_r20.sh` worked well here and can be reused as-is. A
handoff in the same form would be useful again. One note on the runner for next
time: it runs the focused regressions before the workspace suite and exits at the
first failure, so a single known-failing fixture stops it before its two
black-box smokes. Those had to be run separately this round.

## Note on the 400 ms floor

This is recorded in `CLAUDE.md` because the symptom points squarely at ReplayGain
while the cause is the fixture's audio length, and it cost a round of
misdiagnosis here before being measured.
