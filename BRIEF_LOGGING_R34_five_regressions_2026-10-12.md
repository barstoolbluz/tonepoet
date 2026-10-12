# BRIEF — R33/R33C: four regressions, two of them in multichannel SACD

Applied and gated. The nine `r33_` tests pass, so the three items are
implemented as far as those tests reach; nothing has been exercised by hand
here. This is how it reads from the outside, and where our framing is wrong,
trust your own reading.

Gate: **7446 passed / 7 failed**, `r33c_` 1 of 2. Two failures are accounted
for — the expected source-lock freshness failure, since R33 edited `stages.rs`,
and `scanner_treats_opened_then_unpublished_unlocked_ephemeral_descriptor_as_absent`,
which passes alone and is the documented contention flake.

The other five each fail 1 of 1 alone. Four passed on the previous tree. The
fifth is R33C's own new test.

## It did not compile

`TransferCarrier::SacdIso` arrived without its match arm in `src/tui/browse.rs`
— a file outside the delivery's changed set — so the lib failed `E0004` and the
TUI would not start. One arm was added here, in the shape the neighbours use,
naming the XML sidecar rather than the ISO. This is the fifth consecutive
delivery that did not compile.

## 1. Multichannel area selection returns stereo content

```
save_sacd_sidecar_writes_multi_tab_areas_without_implicit_mirror
  left  Some("St-2")        right Some("MC-2")

sidecar_hybrid_disc_routes_mch_tracks_when_mch_area_selected
  left  [StereoT1, StereoT2]   right [MCH T1, MCH T2]
```

Both passed on the previous tree.

**What we want:** the selected area's content is what is read and written.

## 2. An ordered PERFORMER list does not survive

```
multifile_sidecar_untagged_carriers_save_album_performer_and_comment
  the complete ordered PERFORMER list must survive in the inert album block
  left  None
  right Some(["John Bonham", "John Paul Jones", "Jimmy Page", "Robert Plant"])
```

Passed on the previous tree. We think it sits in the ordered-list machinery R33
rewrote for the CUE extension blocks; that connection is our inference.

**What we want:** an ordered multi-value list survives the save intact — the
same property as #76, in another field.

## 3. R33C's own authority test fails

```
r33c_album_view_discnumber_follows_selected_carrier_authority
  src/tui/keybindings.rs:102795
  assertion failed: cue_album_view_sides.iter().all(|side| side.authoritative_target == IndividualFiles)
```

New in R33C. Its sidecar-authority half passes; it fails where it reopens the
same files under individual-files-first and expects every side to resolve to
`IndividualFiles`.

**What we want:** the property it was written to establish holds, or the test
says what does.

## 4. The transfer filter now admits `iso`

`transfer_picker_filter_is_canonical_audio_plus_cue_without_widening_global_audio`
fails because the filter contains `iso` and the expected set does not. Canonical
audio coverage itself did not change, so we read this as the intended #75
widening meeting a test that forbids it, and the expectation as the stale side.
We may have that backwards.

## Open

Requalification is owed; R33 edited `stages.rs`. `R33C_ACCEPTANCE.sh` chains
the focused tests and `scripts/requalify_reference_r20.sh`, and has not been
run — the failures above come first, and it needs a real SACD ISO path.

No part of the SACD work has met an actual disc. The `r33_` tests cover field
preservation, idempotency, stale XML snapshots and unmapped track IDs, and they
pass; the authoring environment had no fixture and we have not run one. Items 1
and 2 are the only other signal there, and both are negative.
