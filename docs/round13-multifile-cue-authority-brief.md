# Round-13 brief — multi-file cue-album authority (real-world corrective to round-12)

## How to read this brief

This brief codifies **desired outcomes and guardrails**. It does **not** prescribe an
implementation — you are the stronger reasoner on the "how." The "What we observed" notes and code
anchors are **context to save you rediscovery time, not instructions**; treat any suggested cause or
recourse as a hint you are free to discard if you find a better path. Optimize for the outcomes and
acceptance criteria below, within the guardrails. If an outcome and a suggested cause seem to
conflict, the outcome wins.

## Context

Round-12 shipped a **configurable aggregate metadata-target priority**
(`aggregate_metadata_target_priority`, an ordered list of `individual-files` / `sidecar-cue` /
`embedded-cue`, resolved first-present-in-order for a directory/album selection). It passed a green
suite (5433/0). There *are* native-multi-FILE tests, but none exercised the combination that breaks
here — a **cue-image album** (each FILE a whole album side/image subdivided into many tracks, carrying
an embedded CUESHEET, alongside a sidecar `.cue`) opened at the folder level under a *non-sidecar-first*
priority, or edited *sidecar-only* with the images left untouched. Real-world use on exactly that
shape exposed two broken behaviors. (Note: some existing tests actively pin the current
image-writing behavior — see "Existing tests to revise" under Acceptance.)

## Baseline

The round-12 changes are currently in the working tree, uncommitted (branch `hardening`, version
0.4.5); these defects live inside that feature, so you build on the current working-tree state.

---

## Desired outcomes (the contracts)

**O1 — Cue-image albums open as albums, not as raw images.** Opening a folder that is a cue-image
album — audio images subdivided into multiple tracks by a sidecar and/or embedded cue — in the
folder-level metadata editor shows the **full multi-track album (every track across every image)**,
under **any** configured priority order (individual-first, sidecar-first, embedded-first). It must
never collapse the album to "one row per image file."

For a cue-image album, "individual files" is **not** a valid authoritative representation (the images
are not loose per-track files) — the authoritative target is chosen among the cue representations
present (sidecar and/or embedded) per the configured order. So under an individual-first order the
sidecar (the next applicable target) becomes authoritative, which is what makes O3 reachable for the
user's actual configuration.

**O2 — Loose per-track folders are unaffected.** A genuine folder of ordinary per-track audio files
(one song per file, no subdividing cue) still resolves to individual-file metadata, exactly as today.

**O3 — Sidecar-authoritative editing never rewrites the images.** For a native multi-file cue album,
when the sidecar is the authoritative target (whether reached by explicitly selecting the sidecar
`.cue`, or by priority resolution), editing and saving **cue-representable album/track metadata**
(title, artist/performer, date, genre, catalog, ISRC, etc. — the fields a `.cue` can hold) writes
**only the sidecar `.cue`** and leaves the audio image files **byte-identical** — no embedded-CUESHEET
regeneration into the images, no tag writes to the images. (The user's whole reason for this feature:
edit album metadata without paying to rewrite gigabyte-scale images.)

Boundary for you to resolve (outcome, not mechanism): the editor can also expose fields a CUE cannot
represent (artwork, ReplayGain, arbitrary custom tags). A sidecar-only save cannot persist those to
the `.cue`. Decide the right behavior — e.g. persist only the non-representable fields to the images
while keeping the cue-representable ones sidecar-only, or clearly refuse/flag such edits — but a
sidecar-authoritative save of the *common* case (album/track text fields) must never rewrite the
images, and must never silently drop a user's edit.

**O4 — Nothing else regresses.** When embedded is the authoritative target, existing embedded-write
behavior is preserved. Single-image and one-track-per-file behavior is unchanged. The round-12 green
suite still passes.

---

## The problems, as observed (context — not a recipe)

### Problem A → outcomes O1/O2: folder open collapses the album to "N images as N tracks"

**Observed.** With priority `individual-files` first, right-click the fixture folder → Tags & Tagging
→ Edit metadata shows the **2 images as the only 2 tracks**, instead of the full 12-track album.
(By code trace, the default `sidecar`-first order avoids this — the pre-resolution is skipped and
admission detects the sidecar — so the breakage is currently order-dependent; but collapsing a
cue-image album to its raw images is wrong under any order.)

**What we found (for your context).** For a directory selection, a pre-resolution
(`resolve_directory_metadata_target_before_sidecar`, keybindings.rs:21757 — mirrored in
`resolve_directory_metadata_target`, 21722) treats `IndividualFiles` as applicable on *"any audio
file present"*, with no test for whether those files are cue-subdivided images. It runs against a raw
directory audio glob (`expand_audio_paths_for_metadata`, command.rs:93 → `expand_paths_to_all_audio`,
queue_expansion.rs:210; the wrapper's comment at command.rs:264 confirms it returns raw audio files —
including single-image CUE carriers — not cue tracks), so `IndividualFiles` wins before the
sidecar/embedded heuristics are consulted. The pre-resolution deliberately exists to avoid the
(more expensive) sidecar admission scan.

**One caution if you explore this area:** `usable_embedded_metadata_surfaces_for_paths`
(keybindings.rs:14643) will **not** detect this fixture — it only accepts single-image writable
embedded cuesheets (`multi_file_read_only: false`, line 14673), and these images carry *multi-FILE*
embedded cuesheets (`multi_file_read_only: true`). A plain presence check like
`metadata_entries_contain_embedded_cuesheet` (17700) *does* fire for them, and the sidecar `.cue` is
of course present in the folder. How you make `IndividualFiles` cue-aware (and how you trade that off
against the pre-resolution's cost-avoidance intent) is your call.

### Problem B → outcome O3: no way to edit a multi-file album without rewriting the images

**Observed.** Right-click the sidecar `.cue` → Edit metadata opens the album but edits/saves via the
**embedded** cues, so saving rewrites the multi-gigabyte `.wv` images (extremely slow). There is no
mode where the sidecar is authoritative for *writing*.

**What we found (for your context).** The native multi-file editor takes cuesheet authority from the
embedded sheets (keybindings.rs:16958/16981/16984), and the save path
(`regenerate_unified_cue_album_cuesheet_for_save`, 14000) persists into the images by setting
`embedded_cuesheet_present = true` (14096) — this part is **pre-existing** (identical in baseline
`a6b8236`); round-12 never wired the new priority into it. `apply_policy_selected_metadata_cue_source`
(14774), which maps policy → sidecar-vs-embedded selection, is single-image-only (errors for
`audio_paths != 1`), so priority cannot select the write target for a multi-file album. A sidecar
writeback does run (`cue_sidecar_writeback_plan_for_state` multi-file branch, 10390) but it is
*additive* — its own message is *"image tags were saved but the sidecar was left unchanged."* A
byte-preserving sidecar writeback engine already exists (`cue-sidecar-writeback`). How to give
multi-file albums a genuine sidecar-only read+write authority is your call.

---

## Guardrails / constraints / scope

**In scope:** only the native multi-FILE (multi-track-per-image) shape, for the outcomes above.

**Explicitly OUT of scope (do not fold in):**
- **WavPack/APE write optimization.** Images are slow to write because the WavPack/APE path uses the
  "conservative generic writer" (full-file backup copy, or a full `save_to` temp rewrite) while FLAC
  (padding-aware) and DSF (native writer) were optimized — probe.rs:5736 notes FLAC's optimized path
  and that "other formats deliberately retain the conservative generic writer" (WavPack/APE among
  them). That is a separate, later item. Achieving O3 (sidecar-only,
  images untouched) makes it unnecessary for this workflow; do not attempt the APE optimization here.
- The Library abstraction. Config-UX for the priority knob (still plumbing-only). Custom-tag-builder /
  paste-tags. Vinyl side-number parsing.

**Guardrails:**
- Single-user desktop TUI. Smallest correct change in the surrounding style. **No** new
  subsystems / journals / transaction / rollback / recovery layers, no adversarial-race / ABA / inode
  hardening, no "frameworks." A recent round was rejected in full for exactly that kind of over-build.
- byobu-safe input: no F-keys, no chord-as-only-path, no emoji / decorative unicode (▸/▾ excepted).
- Version stays 0.4.5.

## Acceptance

Add a fixture of the missing shape — **≥2 image FILEs, each subdivided into ≥2 tracks and carrying an
embedded CUESHEET, plus a matching native multi-FILE sidecar `.cue`** (tiny synthetic audio is fine;
the pathology is structural). Real reference that reproduces both: `/home/daedalus/livetorrents/Guns
N' Roses - Appetite For Destruction [DMM Japan P-13556]` (2× `.wv` sides, each with a 622-byte embedded
Cuesheet; sidecar with tracks 01–06 on side A, 07+ on side B).

Pin these outcomes as tests:
1. **O1**: opening the fixture folder under both individual-first and sidecar-first priority opens the
   full multi-track album (all tracks across both images), never "2 tracks."
2. **O2**: a loose per-track folder still opens as individual files.
3. **O3**: with the sidecar authoritative, saving a **cue-representable** album/track edit (e.g. a
   title or artist change) changes only the `.cue` and leaves both images **byte-identical** (assert
   via hash/mtime). Whatever you decide for non-representable fields, that decision is pinned too.
4. **O4**: embedded-authoritative save behavior and single-image / one-track-per-file behavior
   unchanged; round-12 suite green under `cargo test --workspace --no-fail-fast` (fail-fast hides
   non-lib-crate failures — round-12 learned this the hard way).

**Existing tests to revise (not regressions).** Some current native-multi-file tests pin the *old*
contract in which the images are written on save — most directly
`native_multi_file_sidecar_writeback_waits_for_every_member_image_save` (keybindings.rs:47829), which
asserts the sidecar write "waits for every member image save." O3 (sidecar-only, images untouched)
deliberately changes that. Revise these tests to the new contract; do not preserve the old behavior
just to keep them green, and do not treat their failure as a regression. Audit the other
`native_multi_file*` / `*multi_file*cue*` tests (e.g. `four_file_native_multi_file_album_consolidates_persists_reopens_and_queues_once`,
55192) for the same assumption and update those that encode image-writing on a sidecar-authoritative save.

No compiler on your side — the applying side compile-fixes and runs the full gate.
