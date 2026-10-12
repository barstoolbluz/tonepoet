# BRIEF — R32 is accepted; three field defects, one of them standing

R32 is applied, gated and committed. The info glyph works on both true-peak
rows, the trailing command text is gone, and the help popup reads well.

Gate: **7442 passed / 1 failed**, the failure being
`installed_reference_qualification_binds_the_current_locked_sources` — expected,
since R30 moved `stages.rs` and `settings.rs`. Requalification is owed and is
not an R32 regression. `r32_` 4 of 4, `r31_` 3 of 3, `r30_` 10 of 10. Compile
clean, no warnings.

The two claims R32 was asked to check rather than trust both came back
confirmed, and the `allow_boost` distinction is why the help copy promises only
that each track gets its own gain. That was the right call.

This brief carries three field defects, filed as issues #74, #75 and #76. The
full entries are in `docs/OUTSTANDING_ISSUES.md`; what follows is the shape of
each. We misapprehend this codebase routinely, so where our framing is wrong,
trust your own reading.

## 1. The authoritative metadata settings are not honored — issue #76

Saving tags on a folder of individually tagged FLAC files is refused:

```
save aborted: album-scoped COMPOSER has different values on audio files covered
by the same CUE; make the COMPOSER values match before saving; no carrier was
changed
```

It happens on any various-artists album with several composers. The
configuration asks for individual files first:

```toml
aggregate_metadata_target_priority = [
    "individual-files",
    "sidecar-cue",
    "embedded-cue",
]
```

The save is refused anyway, by a rule belonging to a carrier that was demoted.
The tags then appear written and are still present on reopening, so the claim
that no carrier was changed may also be untrue.

**What is wanted, and has been wanted for four months:** the authoritative
metadata settings are honored. Not this case patched. The setting decides which
carriers are written and in what order, and nothing outside that order refuses,
overrides or quietly writes.

This has arrived as a run of separate edge cases across many rounds. Each fix
held and the next case appeared. The outcome wanted is that authority,
established once, rather than another instance closed.

**Second, and possibly the same work:** if COMPOSER is not currently supported
in TonePoet's extended CUE formatting — the proprietary `REM` comment blocks —
fix that too. Per-track composers are the normal case on a soundtrack and should
survive a round trip through our own CUE extension.

## 2. Transfer tags cannot see an SACD ISO — issue #75

Right-click a folder holding an SACD ISO and its XML sidecar, Tags and tagging,
Transfer tags from, pick a source folder of individually tagged FLAC files:

```
Transfer tags: selected folder contains no audio files
```

The folder does contain audio. The other direction, from an ISO-and-sidecar
folder onto individual files, was not tried and is expected to fail the same
way.

**Fix this.** Tag transfer works in both directions with an SACD ISO as source
or as target. Where it genuinely cannot, the refusal names what it could not
read rather than claiming there is no audio.

## 3. The album gain log does not add up — issue #74

```
  Applied: +6.338115084 dB
  Loudest true peak: -6.338124793 dBTP, across 8 tracks
```

Those do not sum to the ceiling. They are 0.000009709 dB apart and nothing on
the page accounts for the difference, so a reader checking the subtraction
concludes the gain is wrong. The gap is the error allowance that keeps the
output under the ceiling; the log never names it.

**What we want:** a reader can account for every digit in that block.

**And the reason it surfaced.** The true-peak target accepts nine decimals,
finer than the allowance, so a request of 0.00001 dB is below the resolution the
gain can honor. Capping the input fixes that. Five decimals does not clear the
allowance; four does — at five it is one step at 0 dBTP and nearly four steps at
-12 dBTP, while at four decimals it stays inside the last digit across the whole
range. The table is in the issue.

**What we want:** the finest true-peak value a user can ask for is coarser than
the allowance, so the figure entered is the figure applied.

Two things there to check rather than trust. We derived the linear error from
that one log line, not from the qualified terminal bound, so the real figure
should set the cap. And the Reference path's pre-gain term is zero where other
terminals' may not be, so the worst terminal should set it.

## Requalification

Still owed from R30. `R32_ACCEPTANCE.sh` chains the `r32_` tests into
`R31_ACCEPTANCE.sh`, `R30_ACCEPTANCE.sh` and
`scripts/requalify_reference_r20.sh`, and has not been run — it needs a real
SACD ISO path we do not have in hand. Nothing in R32 touched a source-locked
file.

Issue #73, popup markdown rendering, was split out deliberately and is not
asked for here.
