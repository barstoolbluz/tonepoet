# Corrective brief — metadata-autonumber v6 (APE disc round-trip + preflight idempotency)

Status: CORRECTIVE round on your v5
(`metadata_autonumber_corrective_v5_world_class_disk_alias_fixed`). v5 is strong:
it fixed 4 of the 5 v5-input failures, correctly implemented the FLAC/Vorbis-only
lexical decision (capability matrix now `TEXTUAL` for FLAC/Vorbis,
`PLAIN_UNSIGNED_ONLY` for DSF/ID3v2/APE/MP4, `NONE` for DFF/unclassified), repaired
the MP4 reader, re-anchored the filename parser, and reclassified the DSF
eligibility case correctly (it was an already-satisfied `1` row — good catch).

This round fixes **3 failing tests — all your own new hardening tests** (the
idempotency + alias-conflict pass). We ran the full gate with the real toolchain:
**4,738 pass, zero regressions** (all pre-existing + your fixed tests green;
DSF/DSD suite unaffected by the `dsf_tags.rs` changes). Trust your own analysis;
the evidence below is from actual execution.

---

## Failure cluster A — APE (WavPack) disc number does not round-trip

APE *track* number round-trips fine; APE *disc* number reads back empty. Two
tests fail on this one root cause:

**`tui::probe::tests::ape_numbering_alias_conflicts_fail_closed_and_equal_aliases_coalesce`**
(`probe.rs:9404`) — writes `TRACKNUMBER=7, TRACKTOTAL=17, DISCNUMBER=2,
DISCTOTAL=3, TOTALDISCS=3` (aliases that must coalesce) to the APE `.wv` carrier.
`editor_numbering_value(TRACKNUMBER)==7` and `TRACKTOTAL==17` pass; then
`assert_eq!(editor_numbering_value(DISCNUMBER), "2")` fails — got `""`. So the
disc number is not recovered from the APE carrier. Likely your reader's disc
supplement (`disk()` / the "Disc" APE item) doesn't surface for APE the way
`track()` does — verify with the fixture.

**`tui::probe::tests::ape_numbering_capability_matches_production_round_trip`**
(`probe.rs:9019`) — the idempotency assertion:
`fallback_calls == 0` failed (got `1`): "repeating accepted LoftyApe numbering
write must not enter the full-file fallback transaction." This is **downstream of
the disc gap** — if APE disc reads back empty, the no-op check sees the requested
disc as unsatisfied, so the repeat write re-enters the fallback instead of being
recognized as already-satisfied.

**Fix latitude:** either recover APE disc numbering symmetrically with track
(read the APE "Disc"/typed disk value into the editor row), OR fail-close APE
disc numbering explicitly — your call, but track and disc must be *consistent*
(don't offer disc numbering you can't round-trip), and the no-op/idempotency
contract must then hold for whatever APE supports. Keep FLAC/Vorbis unchanged.

## Failure cluster B — preflight re-read loses a concurrent write (format-agnostic)

**`tui::probe::tests::lofty_noop_preflight_never_serializes_a_stale_carrier_snapshot`**
(`probe.rs:10098`) — uses the **ID3v2 `.mp3`** fixture (this is NOT APE-specific).
A fallback hook writes `TITLE="hook update"` concurrently; then the main
`write_all_tags(TRACKNUMBER=7)` runs. The test asserts the concurrent TITLE
survives: `editor_value(TITLE) == Some("hook update")` — got `Some("")`. So your
"re-read after the hook, then reapply the normalized change set" path serialized
a **stale snapshot** that clobbered the hook's TITLE back to empty.

Your v5 report says the full-file path "re-reads the carrier and reapplies the
normalized change set … never serializes a stale preflight snapshot." That
invariant is not holding here: the reapply must serialize from the carrier's
**current** on-disk state (including the concurrent hook mutation), applying only
the requested numbering delta on top — not a snapshot captured before the hook
ran.

---

## Constraints

- Preserve everything else in v5 (capability matrix, MP4 reader repair, parser,
  alias unification, FLAC path). Only the APE disc reader/capability, the APE
  no-op consequence, and the preflight reapply correctness should change.
- Fail-close: never offer/persist a representation a backend can't faithfully
  round-trip. No silent data loss.
- The FLAC live path (`saved_side_prefixed_flac_…`) and the whole pre-existing
  suite must stay green.
- Complete-file delivery; regenerate `docs/handoff_manifest.txt` last. No toolchain
  claims needed — we run the gate.

## Gate we will run on your return

`cargo check --workspace --all-targets` (0 errors) · full
`cargo test --workspace --no-fail-fast` (every `test result:` line 0 failed) ·
zero new warnings · DSD checkers + live DSF→FLAC smoke (since `dsf_tags.rs` is
in play) · FLAC side-prefix path green.
