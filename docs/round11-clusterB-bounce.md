# Round 11 — Cluster-B bounce: the ID3v1-only MP3 all-view tests

**To the reasoning model.** Four of your new item-6 (all-view) tests were applied but are
currently `#[ignore]`d on branch `hardening` because their shared fixture is **impossible
to construct as written**. This is not a request to add machinery — it's a small,
well-scoped fixture/spec correction. Everything else in round 11 is applied and the suite
is green with these four ignored.

## The four ignored tests (in `src/tui/probe.rs`)

- `all_view_edits_unsuffixed_title_in_id3v1_only_mp3_without_creating_id3v2`
- `all_view_deletes_unsuffixed_title_from_id3v1_only_mp3`
- `all_view_new_field_on_id3v1_only_mp3_uses_normal_id3v2_primary`
- `all_view_mixed_mp3_edit_targets_each_files_existing_preferred_container`

All four share `seed_id3v1_only_mp3(path, title)`, which fails at its **own setup sanity
assertion** (`assert!(tagged.tag(TagType::Id3v2).is_none())`) before the feature under test
ever runs.

## Why the fixture is impossible (empirically confirmed)

`seed_id3v1_only_mp3` reads the base fixture (`ID3V2_NUMBERING_FIXTURE`, which **already has
an ID3v2 tag**), calls `remove(TagType::Id3v2)` + `remove(TagType::Id3v1)`, inserts an
ID3v1-only tag, saves with `WriteOptions::default()`, reopens, and asserts no ID3v2 remains.

A direct probe of lofty on this exact sequence produced:

```
base tags:                 [Id3v2]
in-memory before save:     [Id3v1]          (remove worked in memory)
after save + reopen:       [Id3v2, Id3v1]   (on-disk ID3v2 was NOT stripped)
id3v2 present after:       true
```

**Conclusion:** lofty's `save_to_path` with default `WriteOptions` does **not** strip an
ID3v2 tag that exists on disk merely because it was removed from the in-memory
`TaggedFile`. Starting from an ID3v2-bearing fixture, you cannot produce an ID3v1-only MP3
this way.

## Fixture inventory (important — there is NO tagless MP3 to start from)

The repo has exactly **one** MP3 fixture: `tests/fixtures/metadata_persistence/id3v2.mp3`
(bound as `ID3V2_NUMBERING_FIXTURE` in probe.rs:13125), and it **carries an ID3v2 tag**.
There is no tagless / ID3v1-only MP3 fixture anywhere, and no existing helper that
synthesizes a bare MP3. So you cannot simply "start from a tagless base" — one does not
exist. This is why `seed_id3v1_only_mp3`'s remove+save approach fails.

## What we need from you (two parts)

1. **A workable ID3v1-only fixture — you must create the tagless base.** Options, your call:
   (a) **strip the ID3v2 prefix** from `ID3V2_NUMBERING_FIXTURE` to the first MPEG frame sync
   (ID3v2 is a length-prefixed header at the front of the file; the existing
   `detect_flac_stream_offset` is the FLAC analogue — for MP3 the ID3v2 size is the syncsafe
   header length), then write an ID3v1 tag onto those frames; or
   (b) **synthesize a minimal bare MP3** (a few valid MPEG audio frames, no tags) as a new
   helper or a new committed `tests/fixtures/...` file, then add ID3v1; or
   (c) whatever lofty `WriteOptions`/`Probe`/`remove` path actually yields an ID3v1-only file
   if one exists (the default-save path does not — proven above).
   The goal fixture: an MP3 whose only tag is ID3v1, verified by reopen (`tag(Id3v2).is_none()`).

2. **Confirm the feature's intended contract, which these tests are the spec for.** The test
   names assert that all-view editing of an ID3v1-only MP3 **targets the existing preferred
   container without creating an ID3v2** (`..._without_creating_id3v2`,
   `..._uses_normal_id3v2_primary`, `..._targets_each_files_existing_preferred_container`).
   Given lofty's behavior above, state plainly whether that guarantee is achievable through
   the write path item 6 uses, and if so, how the code enforces it. If lofty forces an ID3v2
   on any MP3 write, the tests' expectations (not just the fixture) need to change to match
   the real, achievable behavior — decide and specify which.

## Scope discipline (unchanged from the round-11 handoff)

This is a **fixture + spec** correction. Do **not** add a native ID3v1 writer, a new
transaction/journal/recovery layer, or any machinery beyond what's needed to (a) build a
genuine ID3v1-only fixture and (b) make the four tests assert the real achievable behavior.
Single-user desktop app; no adversary; smallest correct change.

## Applying-side state

Branch `hardening` @ (current round-11 head). The four tests are `#[ignore]`d with an
in-code reason pointing here. Un-ignore them in your corrective delivery once the fixture
and expectations are corrected. The rest of round 11 (items 2a/2b/2c/3/4/5/6/7, the extended
canonical field set) is applied and green.
