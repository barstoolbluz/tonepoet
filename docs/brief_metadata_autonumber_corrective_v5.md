# Corrective brief — metadata-autonumber v5 (capability reality + 2 logic bugs)

Status: CORRECTIVE round on your v4
(`metadata_autonumber_context_menu_world_class_fixed_v4`). Your v4 applied
cleanly and is **excellent** — it compiles (after one trivial fix on our side,
below), introduces **zero regressions** (4,714 pre-existing + new tests pass),
and zero new warnings. The raw-string side-prefix carriage, the parked-editor
rendering fix, the Custom overlay, provenance gating, and the capability
centralization are all preserved and correct.

This round fixes **5 failing tests — all your own new tests** — that your
container couldn't execute. We ran the full gate with the real toolchain +
pinned tools. Trust your own analysis; the evidence below is ground truth from
actual test execution.

**We already fixed (do not redo, it's in the tree you're starting from):** the
non-exhaustive `match button` at `keybindings.rs` screen-dispatch was missing the
5 new `MetadataAutoNumber*` variants; we added them to the existing overlay-owned
no-op arm (alongside `MetadataArtwork*`). That was the only compile error.

---

## Product decision (from the user) — scope of side/lexical numbering

**Side-prefixed and other LEXICAL numbering (values whose exact spelling matters:
`A01`, padded `01`, side `A01/B01`) is FLAC/Vorbis-ONLY for now.** Reason: your
v4 round-trip tests proved that a non-numeric value written through the CURRENT
production write→read path on ID3v2 (`TRCK`), APE (`Track`), and MP4 (`trkn`)
comes back **empty**. We did NOT determine which layer loses it — writer, lofty,
or the production reader — and you should not assume: bug #5 shows the analogous
MP4 loss is a **reader gap** (the write is correct), so the ID3v2/APE loss may be
the reader too. Per the user's decision the layer does not matter for this round —
cross-format lexical support is DEFERRED — so simply align the declared
capabilities to what the current production path **provably** round-trips.
FLAC/Vorbis store `TRACKNUMBER` as free-form text and round-trip everything
(`A01`, `7`, `01/17` all verified green).

So: **make the declared capabilities MATCH what each backend provably
round-trips through the production path, and make menu eligibility + tests
consistent with that.** Fail-close, no silent data loss. Cross-format lexical
support is a deferred feature, not this round.

You own the exact per-backend capability matrix — determine it from the
real-carrier round-trip evidence (your fixtures are committed and correct). Our
findings from running them:

- **FLAC / Vorbis** — full `TEXTUAL` (plain, fraction, lexical/padded/side). ✅ verified round-trips `A01`, `7`, `01/17`.
- **ID3v2 / APE** — declared `TEXTUAL` but **lexical does NOT round-trip through the current path** (`A01` → `""`). Plain numeric (`7`) very likely survives (`TRCK`/`Track` hold it) but we did not reach that assertion — verify with the fixtures; padded (`01`) and fraction (`01/17`) too. Declare only what provably survives. Likely NOT `TEXTUAL`.
- **MP4 ilst** — `PLAIN_UNSIGNED_ONLY`, but see bug #5: the numeric write succeeds (`tag.track()==7`) yet the **editor reader doesn't surface `trkn` as a `TRACKNUMBER` row** (reads back `""`). Either fix the reader so plain numbering works on MP4, or fail-close MP4 numbering and make the test assert that. Your call.
- **DSF** — `PLAIN_UNSIGNED_ONLY`. See bug #1 (eligibility returns nothing).

---

## The 5 failures (exact evidence)

### Real logic bugs (fix the code)

**1. `tui::command::…::command_rejects_side_numbering_on_numeric_carriers_without_mutation`**
(`command.rs:18708`) — `assert_eq!(eligibility.immediate, vec![NumberingScheme::N])`
got `left: []`, `right: [N]`. For a DSF (`PLAIN_UNSIGNED_ONLY`),
`numbering_menu_eligibility(&editor, Track)` returns **no** immediate schemes; it
must include the plain `N` scheme (plain-unsigned is supported). The eligibility
mapping is dropping the plain scheme for numeric-only carriers.

**2. `tui::metadata_autonumber::tests::filename_parser_accepts_side_prefix_and_rejects_embedded_letters`**
(`metadata_autonumber.rs:1342`) —
`assert!(side_number_from_filename("trackA01.flac").is_none())` failed:
`parse_side_number` accepts `"trackA01"`. The side letter must be **anchored at
the start of the stem** (`A01 - …`), not matched when embedded after other
letters (`trackA01`). `"A01 - Come Together"` must still parse (prefix `A`, seq 1);
`"01 - …"` must stay `None`; `"A01/17"` seq 1; `"A01/not-a-total"` `None`.

### Capability/reader reality (align declaration + reader + tests to what round-trips)

**3. `tui::probe::tests::id3v2_numbering_capability_matches_production_round_trip`**
(`probe.rs:8644`) — wrote `"A01"` via the production writer through
`ItemKey::Unknown("TRACKNUMBER")` (normalized to `ItemKey::TrackNumber`), reopened
through `read_all_tags_merged_with_metadata`, expected `"A01"`, got `""` — the
value does not survive the current production write→read path. Which layer drops
it (writer, lofty, or the reader) is for you to determine; cf. bug #5, where the
analogous MP4 loss is a reader gap, not a write failure.

**4. `tui::probe::tests::ape_numbering_capability_matches_production_round_trip`**
(`probe.rs:8644`) — same for APE `Track`: `"A01"` → `""`.

For 3 & 4: since ID3v2/APE are no longer `TEXTUAL`, the
`assert_textual_numbering_backend_round_trip` helper's `["A01", "7", "01/17"]`
loop is wrong for them. Re-shape: assert each backend round-trips exactly the
representations you (re)declare it supports, and assert the ones it does NOT
support are fail-closed (rejected without mutation), not silently written. Keep
the Vorbis assertion exactly as-is (full TEXTUAL).

**5. `tui::probe::tests::mp4_numbering_pairs_round_trip_without_free_form_atoms`**
(`probe.rs:8755`) — numeric write is correct (`tag.track()==Some(7)`,
`track_total==17`, `disk==2`, `disk_total==3` all hold), but
`editor_numbering_value(path, "TRACKNUMBER")` returned `""` — the editor reader
creates a `TRACKNUMBER` row but does not populate it from MP4 `trkn`. Either teach
the reader to surface `trkn`/`disk` numbers as the `TRACKNUMBER`/`DISCNUMBER`
values (so plain `N` numbering works on M4A), or fail-close MP4 auto-numbering and
adjust the test. Your call — but the eligibility, reader, and test must agree.

---

## Constraints (unchanged)

- Preserve everything else in v4. Only the capability declarations, menu
  eligibility, the reader (if you choose to fix MP4), the 2 logic bugs, and the
  affected tests should change.
- Fail-close: a scheme a backend can't faithfully round-trip must be
  unavailable/rejected without mutating the file. No silent coercion.
- Complete-file delivery; regenerate `docs/handoff_manifest.txt` last.
- No toolchain claims needed from you — we run the gate. Just make the tests
  encode the true behavior and the code satisfy them.

## Gate we will run on your return

`cargo check --workspace --all-targets` (0 errors) · full
`cargo test --workspace --no-fail-fast` (every `test result:` line 0 failed) ·
zero *new* warnings vs the pre-existing set · the FLAC live path
(`saved_side_prefixed_flac_…`) stays green.
