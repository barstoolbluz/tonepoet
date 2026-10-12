# BRIEF — R33/R33C landed the three items and broke five tests, two of them in multichannel SACD

R33 and R33C are applied and gated. The nine `r33_` tests pass, so the three
brief items are implemented as far as those tests reach; none of it has been
exercised by hand here. Five tests fail. Four of them passed on the previous
tree; the fifth is R33C's own new test. Two of the four look like real
behaviour regressions in areas the brief did not ask to be touched.

This is how it reads from the outside; we misapprehend this codebase routinely,
so where our framing is wrong, trust your own reading. Everything below we
measured on this host.

## What is verified

Gate: **7446 passed / 7 failed**. Compile clean after one local fix, no
warnings. `r33_` 9 of 9, `r32_` 4 of 4, `r31_` 3 of 3, `r30_` 10 of 10,
`r33c_` 1 of 2.

Two of the seven are accounted for:
`installed_reference_qualification_binds_the_current_locked_sources` is the
expected freshness failure, since R33 edited `stages.rs`; and
`scanner_treats_opened_then_unpublished_unlocked_ephemeral_descriptor_as_absent`
passes in isolation and is the documented contention flake.

The other five each fail 1 of 1 when run alone.

## It did not compile

`TransferCarrier::SacdIso` arrived without its match arm in `src/tui/browse.rs`,
so the lib failed `E0004` and the TUI would not start. One arm was added here,
in the shape the neighbouring arms use, producing a confirmation prompt that
names the XML sidecar rather than the ISO. That file was not in the delivery's
changed set.

This is the fifth consecutive delivery that did not compile, and all five have
been of this kind — a name or a match the compiler would have settled in
seconds.

## 1. Multichannel area selection returns stereo content

```
save_sacd_sidecar_writes_multi_tab_areas_without_implicit_mirror
  left  Some("St-2")
  right Some("MC-2")

sidecar_hybrid_disc_routes_mch_tracks_when_mch_area_selected
  left  [StereoT1, StereoT2]
  right [MCH T1, MCH T2]
```

Both pre-existing: present on the previous tree and passing there. Asking for
the multichannel area appears to yield the stereo area's values.

**What we want:** the selected area's content is the content that is read and
written. The brief asked for SACD tag transfer, not for area routing to change.

## 2. An ordered PERFORMER list does not survive

```
multifile_sidecar_untagged_carriers_save_album_performer_and_comment
  the complete ordered PERFORMER list must survive in the inert album block
  left  None
  right Some(["John Bonham", "John Paul Jones", "Jimmy Page", "Robert Plant"])
```

Pre-existing; it is in `keybindings.rs` on the previous tree and passed there.
We think it sits in the ordered-list machinery R33 rewrote to project performer,
composer, lyricist and arranger credits into the CUE extension blocks, but that
connection is our inference.

**What we want:** an ordered multi-value list survives the save intact. This is
the same property #76 was about, in a different field.

## 3. R33C's own authority test fails

```
r33c_album_view_discnumber_follows_selected_carrier_authority
  src/tui/keybindings.rs:102795
  assertion failed: cue_album_view_sides.iter().all(|side| side.authoritative_target == IndividualFiles)
```

The first half of that test passes. It fails where it reopens the same files
under individual-files-first authority and expects every side to resolve to
`IndividualFiles`.

**What we want:** the property that test was written to establish actually
holds, or the test says what does.

## 4. The transfer filter now admits `iso`

`transfer_picker_filter_is_canonical_audio_plus_cue_without_widening_global_audio`
fails on "transfer filter must equal canonical audio coverage plus only CUE".
The two sets it prints differ by one element: the actual filter contains `iso`
and the expected set does not. (Summarized rather than quoted; the assertion
prints both thirty-element sets.)

We believe this one is the intended #75 behaviour meeting a test that forbids
it: canonical audio coverage itself did not change, so only the transfer filter
widened, which is what admitting an SACD ISO requires. If that reading is
right, the expectation is what should move. We may be wrong about which side is
stale.

## Requalification

Owed. R33 edited `stages.rs`, so the installed qualification no longer binds
this build. `R33C_ACCEPTANCE.sh` chains the focused tests through the R33, R32,
R31 and R30 runners and `scripts/requalify_reference_r20.sh`; it has not been
run, because the failures above come first and it needs a real SACD ISO path we
do not have in hand.

## Still unexercised against a real disc

R33 added `r33_` tests for SACD field preservation, write idempotency, stale
XML snapshot rejection and unmapped track IDs, and they pass. None of that has
met an actual SACD ISO: the delivery notes record that no fixture existed in
the authoring environment, and we have not run one either. Items 1 and 2 are
the only other signal in that area, and both are negative.
