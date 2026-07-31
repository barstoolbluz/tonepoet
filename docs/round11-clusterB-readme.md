# Cluster-B bounce bundle — README

**Governing document:** `docs/round11-clusterB-bounce.md`. Read it first.

This is a small, focused fix — **not** a feature round. Four new item-6 tests
(`all_view_*_mp3` in `src/tui/probe.rs`) are currently `#[ignore]`d on branch
`hardening` @ current head because their shared fixture `seed_id3v1_only_mp3`
is impossible to construct as written (lofty's default save does not strip an
existing on-disk ID3v2; the repo has no tagless MP3 fixture to start from —
see the bounce doc for the empirical probe and the inventory).

## Your task
1. Create a genuine ID3v1-only MP3 fixture (strip the ID3v2 prefix from the
   existing `ID3V2_NUMBERING_FIXTURE`, or synthesize a bare MP3 — see the
   bounce doc's options).
2. Make the four tests assert the **real, achievable** ID3v1-preservation
   behavior of the all-view MP3 edit path, and confirm/specify that contract.
3. Remove the four `#[ignore]` attributes in your delivery.

## Scope discipline (non-negotiable — a prior round-11 attempt was rejected for this)
Fixture + spec correction only. **No** new native ID3v1 writer framework,
transaction/journal/recovery/ownership layer, or adversarial hardening. This is
a single-user desktop audio TUI. Smallest correct change. If you find yourself
building a "system," stop.

## Bundle contents
- `src/tui/probe.rs` — the fixture `seed_id3v1_only_mp3`, the 4 ignored tests,
  the all-view tag read/write path, and `ID3V2_NUMBERING_FIXTURE`
  (`tests/fixtures/metadata_persistence/id3v2.mp3` — the only MP3 fixture; it
  has an ID3v2 tag).
- `src/metadata_persistence.rs` — metadata backend routing the write path uses.
- `docs/round11-clusterB-bounce.md` — the governing brief (probe evidence,
  fixture inventory, the two-part ask).
- `docs/round11-apply-addendum.md` — context on the applied round-11 state
  (includes the Item-2b note: do not re-add move-replay machinery).

## Deliverable
A complete-file overlay or patch (your usual format) with a short report. You
have no compiler; the applying side will compile-fix and run
`cargo test --workspace` inside `nix develop` (baseline 5406/0/19; the 4 tests
should move from ignored to passing). Version stays 0.4.4.
