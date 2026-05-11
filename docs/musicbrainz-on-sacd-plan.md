# MusicBrainz tagging on SACD ISOs — phased plan

Status: drafted 2026-05-11. Phasing is OK to execute over many prompts/sessions; each phase is independently shippable with its own empirical gate.

## Background

tonepoet already has MB integration but it's **TOC-lookup only** (`/ws/2/discid/-?toc=...`), built for CDs/CUEs. SACDs have no Red Book TOC inside the .iso, so the existing path doesn't apply.

The infrastructure that *does* transfer:
- `MetadataEditorState` is a shared in-memory model. Both files and SACDs populate it as `TagEntry` rows. Persistence branches: files → lofty/`ItemKey`, SACDs → `save_sacd_sidecar` → XML.
- `populate_editor_from_mb(state, release)` is backend-agnostic; it writes `TagEntry`s keyed by `display_key` strings. **It already works for SACD editors as-is.**
- `MbSelect` overlay, `release_from_json`, `pick_medium`, the MB JSON parser, `populate_editor_mb_supplemental` — all reusable.

Gaps:
- No text/release search path; only TOC lookup.
- No rate limiter (`musicbrainz.rs` relies on "trust the cache" — fine for single-shot, not for text search + multi-disc + future fingerprinting).
- `editor_key_to_sidecar_key` is a 14-key whitelist that **does not include MUSICBRAINZ_***. Populate writes MB IDs into TagEntries; save silently drops them.
- `build_sacd_editor_state` doesn't surface MUSICBRAINZ_* on read.
- `save_sacd_sidecar` writes to one area only; hybrid mirror not in save path.

User-Agent is already MB-compliant: `tonepoet/<version> (https://github.com/barstoolbluz/tonepoet)`. MB has no UA registry; the UA *is* the identifier. The version-encoded UA is intentional.

The disc-id work shipped in commit `e8c45c2` (canonical MD5 over master TOC, verified bit-perfect against 412/414 sidecars) unblocks sidecar minting. Without it, tonepoet-minted sidecars wouldn't be discoverable by foobar2000 / JRiver.

## Phases

**Dependency graph:**
- **A** (mint-on-save) — unblocks all SACD writes; no deps.
- **B** (MB text search) — no deps; benefits non-SACD too.
- **C** (seeded `:tags-mb`) — depends on A (so user can save tagged sidecars) + B (search path).
- **D** (hybrid mirror) — depends on C (population writes to sidecar).
- **E** (multi-disc disambiguation) — depends on B (multi-medium release data); orthogonal to D.
- **F** (AcoustID, deferred) — depends on C/D existing to even know they're inadequate.

Ship A→B in any order; C requires both. D and E can ship in either order after C/B. F is post-everything.

### Phase A — Mint-on-save (the unblock)

Editing a SACD ISO with no existing sidecar produces a valid `disc.xml` with canonical `<store id>` on first save.

**Work:**
- Add new helper `seed_sidecar_from_scarletbook(&SacdMetadata) -> SidecarMetadata` in `src/tui/sacd_sidecar.rs`. Mirrors the field-extraction logic in `build_sacd_editor_state` but maps into `SidecarMetadata` rather than `MetadataEditorState`. Consumes the full `SacdMetadata` because per-track sidecar rows need area tracks + `SacdText` + `SACD_T_TXT` + `SACD_IGL`, not just master TOC.
- On hybrid discs, synthesize **both** areas' track rows (not just the surfaced area), so later edits to the MCH side don't find empty XML.
- `read_only` gate: when sidecar file is absent, fall back to checking **parent directory** writability. Current `is_writable_path` probes the path passed in and fails fast when the file doesn't exist; the new path probes the parent.
- `save_sacd_sidecar`: takes the ISO path (derive from `state.paths.first()`). If target `disc.xml` doesn't exist, call `mint_disc_id(iso_path)` for `<store id>` and use the seeded sidecar as the base. Existing path preserves the stored id verbatim.
- Status surfacing: "Sidecar created" vs "Sidecar updated".
- Verify pre-existing ScarletBook-level extras (PUBLISHER, COMPOSER if present, etc.) survive seeding.

**Empirical gate (manual on user's machine):** open a bare ISO, edit a title, save. Open resulting `disc.xml` in foobar2000 on a Windows machine (or under Wine if it loads — `foo_input_sacd` uses Windows COM / MSXML2 APIs, so Wine compatibility isn't guaranteed). Confirm the edited title appears in foobar2000's Properties dialog. Structural fallback if foobar2000 unavailable: `sacd-extract --print-meta` on Linux against the same ISO + sidecar combo, asserting our values are echoed.

**Tests (CI):** mint synthesizes valid XML; mint produces canonical id matching `compute_disc_id`; existing-sidecar path doesn't regress.

**Deferred (existing fitfinish memory):** track-count mismatch detection at open; alt-key write-dedup.

---

### Phase B — MusicBrainz text/release search

A new MB query path keyed by artist + album + optional catalog/barcode/year. Returns the same `MbRelease` shape `populate_editor_from_mb` already consumes. Benefits non-SACD too.

**Work:**
- `search_releases_by_query(artist, album, catalog, year) -> Vec<MbRelease>` against `/ws/2/release/?query=...&fmt=json&limit=N`.
- **Lucene-escape helper** for `+ - && || ! ( ) { } [ ] ^ " ~ * ? : \ /`. Apostrophes are NOT Lucene-special; they pass through inside quoted field values. Test: round-trip every track title in the user's library through escape + dry-run query; assert no parse errors and >0 hits on a representative sample.
- **Two-step fetch:** search endpoint returns shallow release metadata only. Full track titles / recording IDs / ISRCs require `GET /ws/2/release/{mbid}?inc=artist-credits+isrcs+labels+recordings+release-groups`. **Response shape differs:** search returns `{releases: [...]}`; detail returns the release object at top level. Existing `release_from_json` consumes the inner shape — the detail-fetch caller must call `release_from_json` on the top-level object directly, not look for a `releases[]` wrapper.
- **Catalog-first query strategy:** when ScarletBook has `album_catalog_number`, the seed query includes `catno:`. Empirical verification: `artist:"Thelonious Monk" AND release:"Solo Monk" AND catno:"SRGS 4520"` returns exactly 1 result; without `catno:` the same query returns 14 candidates with the correct one in top 3. On zero results with catno, fall back to query without it (handles pressings where MB has a stripped/different catalog format).
- **Shared MB rate limiter — one global token, not per-endpoint.** `tokio::sync::Mutex<Instant>` storing the next-allowed time (initialized to `Instant::now()`). Each call awaits until that instant via `tokio::time::sleep_until`, then writes `Instant::now() + Duration::from_secs(1)`. Used by `lookup_release_by_toc`, `search_releases_by_query`, and the per-release detail fetch.
- **MbSelect prefetch architecture (non-trivial):** extend `MbSelectState` with a `BTreeMap<MbReleaseId, FullRelease>` cache. On overlay open, fire a prefetch for the top candidate's full detail. Wire a highlight-change hook in `event_loop` that fires prefetch for the newly-highlighted candidate with a debounce (~150ms) and cancels in-flight prefetches when the cursor moves again. Required because users can't sanity-check picks without per-track titles (the duration-check identity guard doesn't apply to SACDs).
- New cache table `musicbrainz_search_cache` keyed by canonical query string. LRU eviction policy is **duplicated** from `musicbrainz_toc_cache`'s table-specific code at `db.rs:784` — there is no shared `LruCache` abstraction. A future refactor could extract one.

**Empirical gate (verified 2026-05-11):** seed query for Solo Monk:
- With `catno:"SRGS 4520"` → 1 result, the correct Japanese SRGS pressing.
- Without `catno:` → 14 candidates all scoring 100; the correct pressing in the top 3.

**Tests:** Lucene escaping; cache hit/miss; offline JSON fixture parses to `MbRelease`; rate limiter sequencing.

---

### Phase C — Seeded `:tags-mb` from the SACD editor

`:tags-mb` on a SACD editor dispatches the Phase B text search, lands in `MbSelect`, populates editor on selection. Same colon command; dispatch decides between TOC and text based on editor type.

**Work:**
- `command::TagsMb` dispatch: SACD editor → seed `(ARTIST, ALBUM, CATALOGNUMBER, DATE)` from current state, call `search_releases_by_query`. File editor → existing TOC path.
- `:tags-mb <free-form-query>` form for when ScarletBook metadata is wrong/missing (`:tags-mb "miles davis kind of blue"`). `:tags-mb --force` overrides the already-tagged check.
- **`release_already_tagged_on_sidecar(sidecar, release)`** — parallel to `release_already_tagged_on_file`. Reads `MUSICBRAINZ_ALBUMID` from any track's meta (it's album-level — same value on every track). Before populate, if the sidecar already carries the **same** `MUSICBRAINZ_ALBUMID` as the target release, status "already tagged from MB release {mbid}; re-run with `:tags-mb --force` to overwrite." A different `MUSICBRAINZ_ALBUMID` is treated as a retag-from-A-to-B and proceeds without prompting (mirrors existing `release_already_tagged_on_file` semantics). Force form uses `--force` per the existing tonepoet convention (`:embed-cue --force`), not bang-suffix.
- **Widen `editor_key_to_sidecar_key` whitelist** to include:
  - `MUSICBRAINZ_TRACKID` (per-track)
  - `MUSICBRAINZ_RELEASETRACKID` (per-track)
  - `MUSICBRAINZ_ARTISTID` (per-track)
  - `MUSICBRAINZ_ALBUMID` (album-level)
  - `MUSICBRAINZ_ALBUMARTISTID` (album-level)
  - `MUSICBRAINZ_RELEASEGROUPID` (album-level)
  - `ORIGINALDATE` (album-level)
  - `RELEASECOUNTRY` (album-level)
- **Widen `is_album_level_sidecar_key`** to add: `MUSICBRAINZ_ALBUMID`, `MUSICBRAINZ_ALBUMARTISTID`, `MUSICBRAINZ_RELEASEGROUPID`, `ORIGINALDATE`, `RELEASECOUNTRY`.
- **Widen `build_sacd_editor_state` read surface** with explicit per-track vs album-level split (matching the write-side classification):
  - Per-track read via `resolve_per_track(...)`: `MUSICBRAINZ_TRACKID`, `MUSICBRAINZ_RELEASETRACKID`, `MUSICBRAINZ_ARTISTID`.
  - Album-level read via `sidecar_album_value(...)`: `MUSICBRAINZ_ALBUMID`, `MUSICBRAINZ_ALBUMARTISTID`, `MUSICBRAINZ_RELEASEGROUPID`, `ORIGINALDATE`, `RELEASECOUNTRY`.
- **`:mb-back`** wired now (existing backlog item from `project_mb_back_navigation.md` — folds in cleanly since we're in the same code).
- **Track-count mismatch warning** (non-fatal). When `release.tracks.len() != n_tracks`: status "MB release has N tracks, disc has M — wrote what we could." Doesn't block save.

**Empirical gate:** open Solo Monk, `:tags-mb`, pick the matching release, confirm titles populate, `:w`. Reopen in foobar2000; metadata round-trips.

**Tests:**
- Seeding logic with various ScarletBook field-presence combos.
- Sidecar population matches expected key set.
- Foreign-field preservation through MB-populate + save (DISCOGS_*, DR, replaygain, MOOD, STYLE must survive byte-equal after the `IndexMap` migration in non-negotiable #13; value-equality until then).
- Round-trip preservation of MUSICBRAINZ_* on save (env-gated real-sidecar test).
- Already-tagged detection: pre-populated sidecar refuses overwrite without `--force`.

---

### Phase D — Hybrid-area mirror-write

On hybrid SACDs (stereo + MCH), MB population writes the same per-track TITLE/ARTIST/ISRC to **both** areas' track entries. Matches foobar2000's "Link tags between Stereo and Multi-channel areas" preference (default on).

**Work:**
- After computing edits for the surfaced area, apply per-track edits to the **other area's tracks**, keyed by matching track number (not sidecar track id — different namespace).
- Album-level fields replicate to both areas (already replicate within an area).
- **Exclude `DYNAMIC RANGE` and `ALBUM DYNAMIC RANGE`** from the mirror. Matches foobar2000's `is_linked_tag` exclusion: DR is area-specific.
- Track-count divergence between areas: mirror what we can, surface warning. "Wrote tags to stereo + MCH areas" vs "Wrote tags to stereo area only (MCH track count differs)."

**Tests:** parametric test using `tracks_for_area` with hybrid fixture; both areas mutated identically except for DR.

**Deferred:** config toggle `[sacd] mirror_tags_across_areas` (default true). Phase D ships mirror-on as the only behavior.

---

### Phase E — Multi-disc disambiguation

When the selected MB release is multi-disc (Miles Davis box sets, etc.), pick which medium this ISO represents before populating.

**Work:**
- `release_from_json`'s `pick_medium` (musicbrainz.rs:265) currently picks the first medium whose `track-count == n_tracks`. **Insufficient** for 2-disc sets where both discs have the same track count.
- **Two-tier picker:** when `media.len() > 1` and multiple media match the track count, MbSelect expands to a "which disc?" step. Filename auto-pick (stems containing `_1`/`_2`/`disc1`/`disc2`) is a fast path that fires only when the stem unambiguously maps to a single MB medium position; in every other case (no recognizable stem pattern, ambiguous match, conflicting hints) the **manual disc picker is the canonical fallback** and must always be reachable.
- Each ISO in a multi-disc set gets its own sidecar with its own disc id — already correct per the disc-id algorithm.

**Tests:** 2-disc release with auto-pick when stems disambiguate; manual prompt when ambiguous.

---

### Phase F — AcoustID fingerprinting (deferred)

Text search fails on poorly-tagged ISOs or non-Roman scripts. Fingerprint-based ID fills the gap.

**Sketch:** sacd_extract → DSF → ffmpeg PCM decode (44.1 kHz / 16-bit is conventional; Chromaprint resamples internally to 11.025 kHz) → fpcalc → AcoustID API → MB recording IDs → MB release candidates. Heavy: ~minutes per disc; rate-limited; needs AcoustID API key in config.

**Defer** until C/D show real-world inadequacy.

---

## Cross-cutting non-negotiables

1. **Foreign field preservation.** Every save path round-trips the sidecar keys NOT in `editor_key_to_sidecar_key`'s whitelist: `DISCOGS_*`, `DYNAMIC RANGE`, `ALBUM DYNAMIC RANGE`, `STYLE`, `MOOD`, `SUBTITLE`, `ENCODER`, `ENCODED-BY`, `WAVEFORMAT`, and all `<replaygain>` elements. Preservation works by *not touching* them: the save loop iterates editor `TagEntry` rows, translates each via the whitelist, and skips any entry whose `display_key` doesn't translate — leaving the corresponding sidecar entries untouched. Widening the whitelist for MUSICBRAINZ_* (Phase C) doesn't put these foreign keys at risk because they're disjoint from the whitelist additions. **Note:** `PUBLISHER` is *not* a foreign key — it's in the existing whitelist and is managed by the editor; don't add it to this list.

2. **Atomic writes.** `.tmp` + rename. Already in place; new write paths inherit it.

3. **MB User-Agent already compliant.** `tonepoet/<version> (https://github.com/barstoolbluz/tonepoet)`. Don't change without coordinating. The CARGO_PKG_VERSION-encoded UA is intentional — bumps on each release.

4. **Shared MB rate limiter — one global token, not per-endpoint.** Used by all `/ws/2/*` calls. Limit: 1 req/sec/IP per MB policy. Test (CI, virtual time): use `tokio::time::pause` and assert that 5 concurrent dummy calls advance the virtual clock by ≥4 s and fire in submission order. No real-time sleeping in CI.

5. **Read-only fallback with explicit dispatch order.** Sidecar dir not writable → editor stays `read_only=true` and `:tags-mb` shows "cannot save: directory not writable" before any cache lookup AND before any network call. The `read_only` gate is the first check in the `:tags-mb` dispatch. No wasted MB requests.

6. **`<store id>` invariance.** Once written, a sidecar's `<store id>` is never re-minted, even if `:tags-mb` reshapes everything else. Mint only on *first* save.

7. **No bare-char keys.** Every new colon command (`:tags-mb` for SACD, `:tags-mb --force`, `:mb-back`, future `:tags-mb-fingerprint`) ships with click affordance + context menu entry simultaneously. Per `feedback_keyboard_mouse_coeval.md`.

8. **Round-trip preservation of MUSICBRAINZ_* identifiers.** Env-gated test loads a real sidecar with MUSICBRAINZ_*, saves without modifying, asserts **byte-equality** on those keys after the `IndexMap` migration in #13. Asserts **value-equality** during the interim before that migration lands.

9. **Memory updates per phase.** Each phase ends with a memory file update so future-Claude doesn't re-derive what's done.

10. **Track-count divergence handling — two distinct cases.**
    - **MB-populate divergence** (MB release tracks ≠ SACD area tracks): **non-fatal**, write what we can, surface status warning. Same policy for hybrid areas with differing track counts during mirror.
    - **Editor↔sidecar divergence** (`state.paths.len() != area_track_ids.len()` at save time): **hard error**, kept as-is per the existing gate at `keybindings.rs:4296`. This catches a real corruption / desync condition that shouldn't proceed silently.

11. **MbSelect shows enough to verify picks.** Album-level fields visible immediately; per-track titles via prefetch on highlight. SACDs lack the `verify_single_image_matches_release` duration guard, so user-visible track titles are the only sanity-check before commit.

12. **Catalog-first query strategy.** When ScarletBook has catalog number, query includes `catno:`. Falls back to broad query on zero results.

13. **Sidecar validator binary** — binary target, location TBD (new workspace crate, `[[bin]]` in main, or `crates/tonepoet-tools`). Round-trips real sidecars through `parse_sidecar → serialize_sidecar` and asserts **byte-equality**.

    **Prerequisite work (committed, not deferred):** the current `serialize_sidecar` iterates `track.meta` (a `BTreeMap`) in alphabetical order, while real foobar2000-emitted sidecars carry keys in source order. Before the validator can assert byte-equality, `SidecarTrack.meta` migrates from `BTreeMap<String, String>` to `IndexMap<String, String>` so insertion order is preserved, and `<store>` attribute emission is normalized so `type`/`version` round-trip verbatim. This ripples through every consumer of `meta` (parser, serializer, editor key reads/writes, tests) — non-trivial but bounded. Land this as part of (or immediately before) the phase that introduces the validator.

14. **Test corpus shape.** Committed fixtures: ≤5 anonymized real sidecars covering single-area, hybrid, multi-disc, multi-charset (Japanese SACDText). Fixture location TBD (main crate has no `tests/` dir yet; may need creating or living under a workspace crate's `tests/fixtures/`). Anonymize artist/album/track titles to "Artist N" / "Album Title" / "Track N" but preserve every structural quirk (element order, whitespace, escapes, foreign keys).

15. **Two gate tiers, both required.**
    - **Algorithmic gates run in CI**: `mint_disc_id`, `parse_sidecar`, `serialize_sidecar`, `compute_disc_id`, Lucene escape, rate-limiter sequencing (via `tokio::time::pause`), MB JSON parsing, populate dim math. Caught by `cargo test`.
    - **Interop gates are manual**, run on the user's machine: foobar2000 reads sidecars tonepoet wrote; foobar2000-written sidecars (existing in user's library) round-trip through tonepoet without semantic loss. CI cannot run foobar2000.

    The CI/manual split is explicit per phase, not implied.

16. **`:mb-back` lifted from backlog into Phase C** (was `project_mb_back_navigation.md`).

17. **Concurrency** (existing fitfinish item 6): two tonepoet processes writing the same sidecar race on `.disc.xml.tmp`. Acceptable per existing policy. MB-driven save inherits the same atomic-write tempfile pattern.

## Adjacent backlog (not in scope, documented for awareness)

- **`MUSICBRAINZ_ALBUMARTISTID` only captures first artist.** `release_from_json:211` takes `artist-credit[0].artist.id`. Multi-artist compilations lose IDs 2..N. Pre-existing limitation; affects files too.
- **gnudb parity for SACD.** gnudb is CD-TOC-only and has no SACD-aware lookup in the wild. Out of scope.
- **CTDB → MB/Discogs metadata via TOC lookup.** Per `project_ctdb_metadata.md`. Future tagging source.
- **Discogs as an alternative source.** Existing sidecars carry `DISCOGS_*` fields from foobar2000-via-Discogs path. Out of scope; preserve untouched.

## Empirical anchors (already verified, 2026-05-11)

- Canonical `mint_disc_id` matches foobar2000's `<store id>` bit-perfect on 412/414 sidecars in user's library.
- MB text search for Solo Monk: 14 candidates without `catno:`, 1 candidate with `catno:"SRGS 4520"` — catalog-first strategy validated.
- foobar2000's `is_linked_tag` excludes only `dynamic range` and `album dynamic range` from hybrid mirror.
- `populate_editor_from_mb` writes 6 MB-ID keys (TRACKID, RELEASETRACKID, ARTISTID per-track; ALBUMID, ALBUMARTISTID, RELEASEGROUPID album-level) + ISRC (per-track) + ORIGINALDATE (album-level) + RELEASECOUNTRY (album-level) + CATALOGNUMBER (album-level, already in existing whitelist): per-track vs album-level split derived from source.
- SACD editors bypass `single_image` branch (paths.len()=n_tracks>1); go through per-file branch which works correctly when `release.tracks.len() == n_tracks`.

## Files this plan will touch

- `src/tui/musicbrainz.rs` — new `search_releases_by_query`, two-step fetch (search returns `{releases:[]}` shape; detail returns top-level release object), shared global rate limiter, Lucene escape helper.
- `src/tui/sacd_sidecar.rs` — `seed_sidecar_from_scarletbook(&SacdMetadata)` helper; migrate `SidecarTrack.meta` from `BTreeMap` to `IndexMap` for source-order preservation (prerequisite for non-negotiable #13); normalize `<store>` attribute order in `serialize_sidecar`.
- `src/tui/keybindings.rs` — widen `editor_key_to_sidecar_key` and `is_album_level_sidecar_key`; widen `build_sacd_editor_state` read surface with explicit per-track vs album-level split; mint-on-save in `save_sacd_sidecar` (takes iso path from `state.paths.first()`); parent-dir-writability fallback in the `read_only` gate; hybrid mirror in save; `release_already_tagged_on_sidecar` helper.
- `src/tui/command.rs` — `:tags-mb` dispatch by editor type; `:mb-back`; `:tags-mb --force` overrides already-tagged check.
- `src/tui/app.rs` — extend `MbSelectState` with `BTreeMap<MbReleaseId, FullRelease>` prefetch cache and two-tier picker state for medium disambiguation.
- `src/tui/event_loop.rs` — highlight-change hook firing debounced prefetch with cancellation of in-flight requests.
- `src/db.rs` — `musicbrainz_search_cache` table AND duplicated LRU eviction code (no shared `LruCache` abstraction exists; future refactor could extract one).
- Sidecar validator binary — location TBD.
- Fixture corpus — location TBD.
