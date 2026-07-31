# Cluster-B round 2 bounce — the two contract tests (fixture is now correct)

**To the reasoning model.** Your Cluster-B corrective's **fixture fix is correct and
accepted** — `seed_id3v1_only_mp3` now strips the ID3v2 prefix to make a genuinely tagless
base, and **2 of the 4 tests pass** (`all_view_edits_...` and `all_view_mixed_...`). The
remaining two fail, and they are **your part-2 test expectations, not the fixture** — you
reasoned about the all-view MP3 contract without a compiler, and empirical ground truth
(captured on the applying side) contradicts two of the four assertions. Both remaining
tests are re-`#[ignore]`d pending this round.

Version stays 0.4.4. Scope discipline still governs (see the original handoff): this is a
small test-expectation correction plus, for the second one, at most a small typed-key fix.
No new writer framework, transaction/journal/recovery machinery, or adversarial hardening.

## Ground truth (measured, not reasoned)

A diagnostic ran both write scenarios on a real tagless→ID3v1-only MP3 and dumped the
resulting containers:

```
NEW FIELD:  GENRE synthetic row: origin=None, stored_counts=[0]   (correctly "new")
            after write:  tags=[Id3v2, Id3v1]  id3v2.genre=None  id3v1.genre=None
DELETE:     after write:  tags=[]  id3v1.title=None
            read_all_tags TITLE rows = [("", [0])]   (empty value, 0 stored)
```

## Failure 1 — `all_view_deletes_unsuffixed_title_from_id3v1_only_mp3`

**Your test's final assertion is wrong.** The delete fully works: after removing the ID3v1
title, **all tags are gone (`tags=[]`)** and `id3v1.get_string(TrackTitle)` is `None` (your
earlier assertion in the same test passes). But your last assertion requires
`read_all_tags` to contain **no** `TITLE` row — and `read_all_tags` **always synthesizes
empty core-field rows** (TITLE/ARTIST/ALBUM/… via the production `ensure_standard_fields_present`,
whose doc says "so the editor always shows them"). Ground truth: the surviving row is
`("", [0])` — empty value, zero stored values.

**Fix (test-only):** assert the TITLE row **exists but is empty** — value `""` and
`stored_value_count == 0` (i.e. no carrier-backed value) — rather than that the row is
absent. There is no feature defect here; the delete is correct.

## Failure 2 — `all_view_new_field_on_id3v1_only_mp3_uses_normal_id3v2_primary`

**A real, pre-existing MP3 defect that your test exposed — decide test vs feature.** Your
routing reasoning was right (a new row → `normal_primary` = ID3v2), and `editor_tag_origin("GENRE")`
is `None`, `existed` is `false`. But the synthetic GENRE row is keyed as
**`ItemKey::Unknown("GENRE")`** (created by `ensure_standard_fields_present`, which is
**baseline/pre-existing** — it builds every missing standard field with
`ItemKey::Unknown(field)`), **not** the typed `ItemKey::Genre`. The typed mapping already
exists — `item_key_for_new_editor_row("GENRE") => ItemKey::Genre` — but the synthesizer does
not use it. Consequence on MP3: the value is written to a **TXXX** frame, so your typed read
`id3v2.get_string(ItemKey::Genre)` (TCON) returns `None`. (On Vorbis/FLAC this is invisible —
both map to the `GENRE` field — which is why it went unnoticed.)

**Your call, two options:**
- **Feature fix (preferred if in scope):** make `ensure_standard_fields_present` key
  synthesized standard fields via `item_key_for_new_editor_row(field)` (typed) instead of
  `ItemKey::Unknown(field)`, so a newly-filled GENRE lands in the proper typed frame (TCON on
  MP3). This is a genuine correctness improvement, but it is **pre-existing production code
  touched on all formats** — verify it doesn't disturb existing standard-field behavior
  (there are existing passing tests around this helper; keep them green). Keep it minimal.
- **Test-only:** if you judge the TXXX representation acceptable, assert the value is present
  under the Unknown/`GENRE` spelling rather than the typed `ItemKey::Genre`.

Pick one and state the contract you settled on.

## Deliverable

A patch (or complete-file overlay) against `src/tui/probe.rs` — and, if you take the feature
option for Failure 2, the minimal typed-key change to `ensure_standard_fields_present`.
Remove the two round-2 `#[ignore]` attributes. You have no compiler; the applying side runs
`cargo test --workspace` inside `nix develop`. Current baseline with these two ignored is
5408 passed / 0 failed / 17 ignored; success is the two moving to passing (5410 / 0 / 15)
without regressing any existing standard-field test.
