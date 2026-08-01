# Round 12 brief — COMMENT preservation, metadata autocomplete, configurable aggregate metadata-target policy

Prepared by Claude Code (the applying/auditing model) for the reasoning model. Empirical
observations + current behaviour with `file:line` anchors + what should happen. Anchors were
mapped against branch `hardening` == `main` @ `a6b8236`, **version 0.4.5** (round 11 shipped:
items 2a–7, canonical view, Cluster-B). `cargo test --workspace` inside `nix develop` is green
at **5410 / 0 / 15** (56 targets). Preserve that.

> ⛔ **HARD SCOPE DISCIPLINE — read before implementing.** A recent round was rejected in full
> for over-engineering (a few-line file-move fallback became a protocol-v3 crash-recovery
> transaction system). tonepoet is a **single-user desktop audio TUI** — no adversary, no
> concurrent writers, no fleet. Implement the scoped behaviour and nothing more: no new
> transaction/journal/recovery/ownership machinery, no defenses against threats that don't exist
> here, no arcane-edge hardening. The smallest correct change wins. If you catch yourself
> building a "system," stop.

**How to read this brief.** You are a stronger reasoner than the person you work for and than the
model that wrote this. The root-cause analyses are findings from reading + running the code, not
verdicts; the "what should happen" notes describe desired *behaviour*, not a required
implementation. If your analysis discloses a more likely or more fundamental root cause, trust it
and say so. `file:line` anchors are search aids. Every new/changed behaviour needs a pin; changed
behaviour updates its pins rather than deleting them; never truncate gate output.

---

## Item 1 — COMMENT (and other image tags) dropped when converting a single-image + sidecar-CUE album

### Empirical reproduction (measured, not reasoned)

Converting `/home/daedalus/livetorrents/Supertramp - Crime Of The Century (Japan AML-225) (1974)`
→ `/home/daedalus/temp/Supertramp - Crime of the Century (1974) [FLAC] {Japan A&M AML-225 LP  32-192}`.
The **source is one WavPack image (`.wv`) + a sidecar `.cue`** — not a folder of per-track FLACs. The
image's APE tags carry a long `COMMENT`:

```
# ffprobe on the .wv image:
TAG:Album=Crime of the Century (Japan A&M AML-225 LP / 32-192)
TAG:Album Artist=Supertramp
TAG:DATE=1974
TAG:Genre=Prog-Rock; Prog; Progressive Rock; Art Rock; AOR
TAG:COMMENT=Japan A&M-King Records AML-225 First-Press LP | Issued with obi & 4 page insert ... | ...
```

Every output FLAC (`01 - School.flac`, …) has `ALBUM, ALBUMARTIST, ARTIST, CATALOG, CATALOGNUMBER,
DATE, GENRE, TITLE` — **but zero `COMMENT` lines**. The sidecar `.cue` does *not* carry the comment
(it has REM DATE/GENRE/free-text REM lines, PERFORMER, TITLE, CATALOG), so `COMMENT` lives only in
the **image's APE tags**.

### Verified root cause — it is a READ-side allowlist drop, not a writer problem

The conversion goes through the **CUE-image materializer** (`src/convert/pipeline/materializer_cue.rs`),
which reads the image's album-level tags in `read_image_album_metadata` (materializer_cue.rs:2784).
That function classifies each image tag through **two allowlists** and **drops anything in neither**:

- `cue_image_tag_field` (materializer_cue.rs ~2858) → typed fields: only
  `album / albumartist / artist / genre / date / discnumber / totaldiscs`.
- `cue_image_extra_key` (materializer_cue.rs ~2771) → passthrough extras: only
  `catalog(number) / releasecountry / originalyear / originaldate / musicbrainz_*`.
- The read loop's `None => { if let Some(extra_key) = cue_image_extra_key(&key) { … } }` arm
  (materializer_cue.rs ~2823) means **any key matched by neither allowlist is silently dropped**.

`"comment"` (and `"description"`) is in **neither** allowlist, and `ImageAlbumMetadata`
(materializer_cue.rs:2679) has **no comment field**. So the image's COMMENT never enters the
metadata model — it is gone before any writer runs. The output fields that *do* survive are exactly
the allowlisted ones, which is why the bug looks like "everything except COMMENT."

**This same allowlist silently drops other real fields on the CUE path too:** `COMPOSER`,
`PUBLISHER`/`LABEL`, `COPYRIGHT`, `ISRC`, `BARCODE`, `CONDUCTOR`, and any custom tag (`LINEAGE`,
`DISCOGS_URL`, …) present on the image.

### The consistency gap the user is asking about

The user recalls "we fixed a similar problem earlier, on a different materializer/path." The relevant
contrast is: the **single-file** materializer preserves *all* text tags. In
`src/convert/pipeline/materializer_single.rs:411` it iterates every tag item and passes each through
to `extra` via `insert_source_text_tag` (no allowlist):

```rust
for item in tag.items() {
    if let lofty::tag::ItemValue::Text(text) = item.value() {
        let key = item_key_to_extra_key(item.key(), tag_type);
        insert_source_text_tag(&mut extra, &key, text);   // passthrough — COMMENT and custom tags survive
    }
}
```

So a single-file conversion keeps COMMENT and arbitrary custom tags, while the **CUE-image path uses a
narrow allowlist and drops them.** That is the inconsistency to resolve.

### What should happen

Make the CUE-image path preserve image tags consistently with the single-file path — i.e. carry
non-recognized image text tags through (so COMMENT, COMPOSER, PUBLISHER, COPYRIGHT, ISRC, custom keys
etc. survive), rather than dropping everything outside two hard-coded allowlists. Recognized fields
can still be promoted to typed slots; the change is that *unrecognized* text tags should pass through
to metadata rather than vanish. Preserve the existing conflict/merge behaviour across multiple images
(`merge_image_album_metadata`, materializer_cue.rs:2691) and the album-vs-track distinction.

**Secondary write-side consideration (verify before relying on it).** Once COMMENT is carried on the
CUE path, confirm it actually reaches the output tags. The authoritative writer
`authoritative_metadata_tags` (`src/convert/pipeline/stages.rs` ~4665) emits COMMENT only from the
**typed** `meta.comment` field (stages.rs:4668–4675), and COMMENT is listed in
`AUTHORITATIVE_CUE_MANAGED_TAG_KEYS` (stages.rs ~4905) → so the raw source-text re-emission path
(stages.rs ~4782) treats it as *writer-owned* and skips it. Note that `ALBUM`/`ALBUMARTIST` have an
explicit extra-map fallback (stages.rs ~4578) that `COMMENT`/`COMPOSER`/`PUBLISHER`/`COPYRIGHT` do
**not** — so simply landing COMMENT in an `extra` map may not be sufficient; it likely needs to reach
the typed `meta.comment` (and/or an `AlbumMetadata` comment path — note `AlbumMetadata` currently has
no typed comment field, only `TrackMetadata` does). Decide the cleanest routing so the writer emits
it. `CueImage` IS in `source_needs_authoritative_metadata`, so this writer path is the one in play.

Prior precedent for consistency: the `"DESCRIPTION" | "COMMENT" => "COMMENT"` canonical alias in
`src/metadata_persistence.rs:339`, and the `ALBUMARTIST` extra-map fallback pattern (stages.rs ~4578).

**Relevant files:** `src/convert/pipeline/materializer_cue.rs` (read/allowlist),
`src/convert/pipeline/materializer_single.rs` (the passthrough precedent),
`src/convert/pipeline/stages.rs` (the writer), `src/convert/pipeline/types.rs`
(`TrackMetadata`/`AlbumMetadata`). Empirical fixtures live at the paths above — do not modify the
user's source folder.

---

## Item 2 — Autocomplete for token fields in the metadata-editing overlay

Two capabilities:
(a) autocomplete custom tag **field names** when adding a field (the user almost always creates
`DISCOGS_URL`, `LINEAGE`);
(b) autocomplete **values** for artist, album artist, genre, performer, country, composer.

### There is already a completion framework to extend (do not build a new one)

`crates/tui-file-picker/src/text_input.rs` has:
- `CompletionMode` enum (text_input.rs:162) — currently `None | Path | TemplateVariable`.
- `apply_tab_completion(input, mode)` (text_input.rs:1216) dispatching to `apply_path_completion`
  (1228) and `apply_template_variable_completion` (1245).

So item 2 = add a candidate-list completion mode (complete against a provided `&[&str]` or
`&[String]`, common-prefix + cycle), and wire it into the two overlay entry points. Reuse this
plumbing; don't reinvent it.

### The two hook points + Tab availability

- **Add custom field name** (capability a): `metadata_editor_open_add` (`src/tui/keybindings.rs:9447`)
  enters `MetadataEditorPhase::AddingKey` (`src/tui/app.rs:5942`) with `add_key_input:
  Option<TextInputState>` (app.rs:7690). The AddingKey key handler
  (`src/tui/keybindings.rs:13714`) does **not** bind Tab — Tab is free here for completion.
- **Edit a field value inline** (capability b): `metadata_editor_begin_cursor_value_edit`
  (`src/tui/keybindings.rs:9623`) enters `MetadataEditorPhase::InlineEdit` with `edit_input:
  Option<TextInputState>` (app.rs:7689). In this handler (`src/tui/keybindings.rs:13646`), **Tab is
  already bound to commit-and-advance** (keybindings.rs:13660). So value completion needs a different
  affordance — e.g. a suggestion popup with arrow/Enter, or a non-Tab accept key. Your call on the
  UX; respect the byobu-safe input rule (no F-keys; don't make a chord the only path).

The field being edited is known at edit time: the cursor points at a `TagEntry` with `display_key`
(e.g. `"ARTIST"`, `"GENRE"`, `"LINEAGE"`) and `item_key` (`src/tui/probe.rs` `TagEntry`, fields
~5909/5911), reachable via `state.active_surface().entries[state.cursor]`. Use `display_key` to
select the right candidate source per field.

### Completion sources — what is embedded vs what is not (honest inventory)

- **Field-name candidates (capability a):** the extended canonical list `STANDARD_KEY_ORDER`
  (`src/tui/probe.rs:7157`, 34 entries incl. `LINEAGE`, `DISCOGS_URL`) + `CORE_EDITOR_FIELDS`
  (`src/tui/probe.rs:7199`) are the natural static candidate set. Add the user's frequent custom keys.
- **Artist + Album Artist values:** embedded. `docs/canonical_artists_reference.txt` (2,437 names) is
  `include_str!`-loaded into `ArtistCanonicalizer` (`src/convert/pipeline/label_resolver.rs:15`,
  struct ~599, `LazyLock` ARTIST_CANONICALIZER ~628). Reuse its list for both fields (there is no
  separate album-artist list; same names apply).
- **Country values:** embedded. `DictionaryLabelResolver` (`src/convert/pipeline/label_resolver.rs`)
  has canonical country variants (~253) usable as a country candidate list.
- **Genre, Composer, Performer:** **NO embedded list exists** (verified). Options for these: ship a
  small curated static list (e.g. common genres), and/or learn from user history (values the user has
  previously entered / tags present across their library). Flagging this honestly so you decide the
  source rather than assume one exists. The user believed the artist list is embedded — correct — but
  did not realize genre/composer/performer have no embedded source.

**Relevant files:** `crates/tui-file-picker/src/text_input.rs`, `src/tui/keybindings.rs`,
`src/tui/app.rs`, `src/tui/draw_overlays.rs` (render the suggestion affordance),
`src/convert/pipeline/label_resolver.rs`, `docs/canonical_artists_reference.txt`.

---

## Item 3 — Configurable ordered priority for aggregate-level (directory/album) metadata targets

This realizes the user's standing design stake: a directory (and later a Library album) is a *logical
audio set*; the user configures an **ordered preference** among three metadata targets and tonepoet
resolves the first available+applicable one when the aggregate is selected — replacing today's fixed
heuristics. The full requirement is the user's spec (reproduced faithfully below). Config plumbing
must be **decoupled from presentation** (UX later) and the resolver must be **reusable by a future
Library album** (do NOT build the Library this round).

### The three targets and the ordering

Targets: (1) individual audio files, (2) external **sidecar CUE**, (3) **embedded CUE** in an audio
image. The user picks any order, e.g. `sidecar-cue > embedded-cue > individual-files`. On directory
select, inspect the directory and write metadata to the **first present+applicable** target in that
order. Examples of valid orders:
- individual files → sidecar CUE → embedded CUE
- sidecar CUE → embedded CUE → individual files
- embedded CUE → sidecar CUE → individual files

### This GENERALIZES existing machinery — extend, don't rebuild

The three targets are **already modeled** by the `TransferCarrier` enum
(`src/tui/tag_interchange.rs:42`): `Files { paths } | SidecarCue { … } | EmbeddedCue { … }`. And a
**2-way** policy already exists — `CueSidecarPolicy` (`IgnoreCue | SidecarOnly | PreferSidecar |
PreferEmbedded | EmbeddedOnly`) consumed by `resolve_metadata_cue_source`
(`src/tui/keybindings.rs:14475`, policy match ~14518) and by the current directory classifier
`classify_single_transfer_root` (`src/tui/keybindings.rs:15179`, the heuristic decision tree ~15199–15282;
the current default `PreferSidecar` comes from `src/tui/cue_parser.rs:79`).

So item 3 = **generalize the 2-way sidecar-vs-embedded policy into a 3-way ordered priority
(files/sidecar/embedded)**, and refactor `classify_single_transfer_root` to resolve a
`TransferCarrier` by iterating the configured order against availability, instead of the current fixed
heuristics (multi-file → always sidecar; single-file → embedded-if-present-else-files; policy fixed to
PreferSidecar). Extract the resolution as a **pure function** decoupled from the transfer/picker UI, so
the future Library can reuse it, e.g. `resolve_metadata_write_target(dir, &priority, cancel) ->
TransferCarrier`.

### Detection primitives already exist (for "is this target present?")

- Sidecar CUE present: `find_sidecar_cue` (`src/convert/cue_parser.rs:2041`).
- Embedded CUE present: `read_embedded_cuesheet` (`src/convert/pipeline/materializer_cue.rs:832`);
  queue-side variant `read_embedded_cuesheet_text_for_queue` (`src/convert/queue_expansion.rs:1252`).
- Single audio image vs multi-file: `detect_single_image` / `single_image_info_for_cue`
  (`src/tui/cue_parser.rs:375`), and `parsed_sheet_is_native_multi_file_candidate`
  (`src/tui/cue_parser.rs:81`).

### The three write paths (already exist)

- Individual files: `write_all_tags` (`src/tui/probe.rs:8836`) and the editor batch entry
  `apply_metadata_editor_tag_changes_with_save_blocks_progress_and_forced_deletes_at_verification`
  (`src/tui/probe.rs:8528`, iterates each path).
- Sidecar CUE: `rewrite_cue_sidecar_metadata_from_cuesheet` (`src/convert/cue_parser.rs:467`).
- Embedded CUE: written as a CUESHEET tag via `write_all_tags`; compose via
  `compose_cue_metadata_replacement` (cue_parser.rs) / `apply_embedded_cuesheet_per_track`
  (`src/tui/keybindings.rs:17147`).

### Config plumbing (decoupled from UX)

Add an ordered setting following the existing pattern in `src/config.rs` (the `ConversionSettings`
struct + `#[serde(default = "fn")]` + `Default` impl; e.g. the existing `append_lineage_to_comment`
field at config.rs:390 and `generate_cue_files`/`cue_generation_mode` at ~362). Model it as an ordered
`Vec` of a small typed enum (e.g. `MetadataTarget::{IndividualFiles, SidecarCue, EmbeddedCue}`), not
loose strings, so consumers match exhaustively. Provide a sensible default order. Do NOT invest in
Config UX/presentation this round — the user is explicit that Config is a deliberate grab-bag for now;
just get the plumbing in and threaded to the resolver, cleanly separable from any UI.

### Single-image fallback (first-value collapse)

When priority falls through to individual-file tagging and the directory is a single image without a
usable CUE, write file-level tags to that image; multi-value/track-specific fields collapse to the
FIRST track's value. The precedent is `derive_album_metadata` (`src/convert/pipeline/materializer_archive.rs:1854`),
which uses `tracks[0]` as the anchor when tracks disagree. Reuse that collapse behaviour; don't invent
a new one.

### Explicit file selection MUST be preserved (do not regress)

Directory selection = aggregate policy. But entering a directory and explicitly selecting a sidecar
CUE, an image with an embedded cue, one track, several tracks, or all files must act on **those
selected items**, bypassing the directory-level policy. Selection state exists:
`FilePickerSelectionMode` (`crates/tui-file-picker/src/state.rs:26`), `multi_selected` in both the
picker (state.rs ~1034) and browse (`src/tui/browse.rs:2397`), the "Select Folder" vs "Select File(s)"
hint (`crates/tui-file-picker/src/render.rs:1059`). Conversion already does explicit-vs-directory
selection (`handle_browse_convert_expansion_complete` `src/tui/command.rs:810`,
`expand_paths_to_all_audio` `src/convert/queue_expansion.rs:210`) — keep that intact. (Note the design
memory's old "picker returns one path" gap is partly closed — multi-select is tracked now; verify
multi-select completion end-to-end where the policy needs it.)

### Conversion vs metadata are SEPARATE policies

The same aggregate abstraction applies when a directory is selected for **conversion**, but
metadata-target priority and conversion-source selection are **separate policies** unless explicitly
defined as identical. Do not fuse them. Existing aggregate context to lean on:
`AlbumBatchContext` (`src/convert/pipeline/types.rs:129`) already groups conversion jobs by source
directory (`source_grouping_root`, `source_paths`).

### Reusable-for-Library requirement

Do NOT implement the Library. DO avoid coupling the aggregate-level metadata/conversion model to
directory-picker UI concepts — the resolver + target model should be a plain function over a set of
filesystem paths, so a future album-level Library can call the same operations. `TransferCarrier` +
the pure `resolve_metadata_write_target(...)` are the reuse seam.

### The user's full specification (authoritative — implement to this)

> The setting defines an ordered preference among: (1) individual audio files, (2) an external sidecar
> CUE file, (3) a CUE sheet embedded in an audio image. The user arranges these in any priority order.
> When a directory is selected, tonepoet inspects it and chooses the first available and applicable
> target in the configured order (e.g. order sidecar > embedded > individual: write sidecar if present,
> else embedded if present, else individual files). This replaces the current internal-heuristic
> default.
>
> Folder-level: selecting a directory operates on the directory as a logical audio set; the configured
> priority determines which representation is edited (sidecar → sidecar file; embedded → embedded sheet;
> individual → the audio file(s)). The same aggregate abstraction is used when a directory is selected
> for conversion, though metadata-target priority and conversion-source selection remain separate
> policies unless explicitly identical.
>
> Single-image fallback: if priority falls through to individual-file tagging on a single image without
> a usable sidecar/embedded cue, write metadata to that image; multi-value/track-specific fields use the
> existing first-value fallback.
>
> Explicit file-level behaviour: the directory default must not prevent operating on particular files.
> A user can enter a directory and explicitly select a sidecar CUE, an image with embedded cue, one
> track, several tracks, every file, or any supported combination — and tonepoet acts on those selected
> items rather than the directory-level policy. (This already exists for conversion; the new work adds
> aggregate directory-level behaviour without changing the explicit workflow.)
>
> Future Library: an album selected in the Library invokes the aggregate policy; opening an album and
> selecting files invokes explicit item-level behaviour. Album ≈ directory. This iteration does not build
> the Library but must not couple the aggregate model to directory-picker UI.
>
> Scope: implement (a) configurable ordered priority for aggregate metadata targets; (b) resolution on
> directory select; (c) directory-level metadata ops via the resolved target; (d) directory-level
> conversion with clearly-defined conversion-source rules; (e) preservation of explicit file-selection
> workflows; (f) an internal abstraction reusable by future Library albums. Do not build the Library.

---

## Fences (do NOT fold in unasked)

- The Library itself (item 3 builds the reusable model only).
- Config UX/presentation prettification (plumbing only this round, per the user).
- Custom tag builder + Paste tags (still queued for a later round).
- Vinyl side-number parsing / pairing-guard relaxation (parked).

## Gate

`nix develop --command cargo test --workspace` — baseline 5410 / 0 / 15 across 56 targets. New
behaviour needs pins; no existing test may regress. Version stays 0.4.5. NO F-keys; byobu-safe input;
no emoji/decorative unicode (the ▸/▾ pane indicators are the sanctioned exception).
