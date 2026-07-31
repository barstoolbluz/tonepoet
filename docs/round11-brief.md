# Round 11 brief — tagging UX, source-format honesty, move-undo, and repair tooling

> ⛔ **Before implementing, read the "HARD SCOPE DISCIPLINE" section at the top of
> `docs/round11-handoff-readme.md`.** A prior pass at this brief was rejected in full for
> over-engineering (it turned the few-line item-2a fallback into a crash-recovery transaction
> system). Implement the scoped behaviour and nothing more; no new subsystems/protocols/
> journals; do not harden arcane edges; smallest correct change wins. Stronger reasoner =
> simpler, not more elaborate.

Prepared by Claude Code (the applying/auditing model) for the reasoning model.
Empirical observations + current behaviour with `file:line` anchors + what should
happen. Anchors were mapped against branch `hardening` at `90c2b96` (== `main`
after rounds 8–10 merged; version 0.4.4). Do not treat the anchors as prescriptive
— they exist so you can find the code fast and decide the implementation.

The workspace suite is green at this commit: `cargo test --workspace` = 56 targets,
5384 passed / 0 failed / 15 ignored (run inside `nix develop`, never plain
`cargo test`). Preserve that. New behaviour needs pins; changed behaviour needs its
pins updated, not deleted.

**How to read this brief — please internalise:** you are a stronger reasoner than the
person you are working for, and stronger than the model that wrote this brief. The
root-cause analyses below are *findings from reading the code*, not verdicts, and the
"what should happen" notes describe the desired **behaviour/outcome**, not a required
implementation. If your own analysis discloses a more likely — or a more fundamental
(Ur-) — root cause than what is written here, **trust your findings and follow them**;
say so and proceed. Where this brief names a specific mechanism or an existing function
to reuse, treat it as a pointer to save search time and to show what already exists to
build on — the *approach is yours*. The `file:line` anchors exist to help you locate
code fast, nothing more. Push back on anything here that is wrong.

---

## Part A — Addendum: test edits Claude Code made to your round-10 overlay

Your round-10 v6 overlay was applied and is on `main`. On first real compilation +
`cargo test --workspace`, five of your own new tests failed (you have no compiler,
so these could not be caught before). Four were fixed by editing **your tests** (not
production); one exposed a real production bug that was fixed in production. This
addendum records exactly what changed so your model of the tree stays accurate.

Also fixed (compile-scope, out-of-bundle files you could not see): the archive
wrapper `read_track_metadata_with_warnings` in `materializer_archive.rs` was adapted
to your new 3-tuple return (it discards the recovery flag — §1 authority is scoped to
`SingleFile`); `tonepoet-pipeline/tests/settings_fingerprint.rs` had its exhaustive
`flac_md5_sentinel` literal given `dither_explicit: false`, `SETTINGS_FINGERPRINT_FIELD_COUNT`
bumped 70→71, serde key counts 80→81 / 87→88, and a `dither_explicit` mutation entry
added; and the orphaned naive helper `command_records_contain_dither` was deleted
(zero callers; superseded by your target-aware `command_records_prove_dither_*` family).

### A1 — `dsd_source_rate_target_source_logs_planner_default_pcm_rate` (production correct; test was under-specified)
Your test asserted `Resampling: yes (sox_ng rate, Reference policy, DSD64 → 88.2kHz)`
but never set `req.settings.dsd = DsdSettings::native_v2()`. The "Reference policy"
labels are gated by `selects_reference_dsd_to_pcm` (`tonepoet-pipeline/src/plan.rs:63`),
which requires `is_native_v2()`. Without it the planner treated the source as an ordinary
SoX DSD→PCM conversion. **Fix:** added `req.settings.dsd = DsdSettings::native_v2()`.
Take-away for you: reference-policy log wording is *settings-driven*, not inferred from
the command record.

### A2 — `soxr_settings_use_precise_labels_and_do_not_invent_stopband` (production correct; assertion too narrow)
The shared log fixture `log_test_source()` has **two** tracks (44.1 kHz + 96 kHz).
Targeting 48 kHz, the transition label is a sorted `BTreeSet` → `44.1kHz → 48kHz,
96kHz → 48kHz`. Your assertion pinned only `…phase_shift=45, 96kHz → 48kHz)`, which
never appears. **Fix:** updated the expected string to the full two-transition line.
Take-away: that fixture is multi-track; don't pin a single transition tail.

### A3 — `dsd_targets_never_forward_pcm_dither_explicitness` (feature correct; test setup incomplete)
Selecting `AudioFormat::Dsf` without arming a DSD sample rate left the pill at the PCM
default 44100, so `format_state_to_pipeline_settings` returned
`Err("44100 is not a supported DSD target rate")` and `.unwrap()` panicked before the
guard under test ran. **Fix:** call `format.apply_format_constraints()` (arms a DSD
rate) and set `dither_overridden = true` *after* the cascade so the `!is_dsd &&
dither_overridden` guard is genuinely exercised.

### A4 — `invalid_ape_fallback_marks_recovery_and_preserves_full_valid_tag_set` (REAL §1 production bug — fixed in production)
`total_tracks` came back 1, expected 7. `derive_single_file_album_metadata`
(`src/convert/pipeline/materializer_single.rs`) read the recovered total from the
ordinary `extra` map under lowercase keys (`"tracktotal"`/`"totaltracks"`), which the
fallback reader never populates — the recovered totals live only in the immutable
NUL-prefixed snapshot under canonical UPPERCASE keys. **Fix (production):** source
`total_tracks`/`total_discs` from the snapshot via
`fallback_source_tag_value(&track.metadata.extra, "TRACKTOTAL"/"TOTALTRACKS")` and
`DISCTOTAL`/`TOTALDISCS`, gated on `metadata_recovered_by_fallback`. Healthy sources are
untouched. Note for you: the snapshot is the source of truth for recovered numeric
fields; the ordinary `extra` map is enrichment-mutable.

### A5 — `ape_numbering_capability_matches_production_round_trip` (§5; test premise wrong — rewritten)
The test wrote a fraction (`7/17`) into a *single* number field and demanded the exact
combined spelling back. tonepoet decomposes numbering into a **number + total pair**
(consistent across every carrier and non-lossy: `7/17` → `TRACKNUMBER=7` + `TRACKTOTAL=17`;
a re-save recombines; other apps read the stored value directly). The test was rewritten
(empirically, one fresh fixture per case) to assert the real split behaviour:
- number fields (`TRACKNUMBER`/`DISCNUMBER`) accept plain / zero-padded / fraction;
  fractions split into number + total;
- total fields (`TRACKTOTAL`/`DISCTOTAL`) accept plain / zero-padded counts only — a
  total is a count, not a fraction, so fraction-into-total is invalid input and is not a
  supported round trip;
- side-prefixed lexical (`A01`) is refused per field without mutating the carrier or
  allocating a rollback backup.

Two follow-ups discovered while doing A5, carried into Item 1 below.

---

## Part B — Round 11 work items

### Item 1 — Verify/close-out the APE numbering (A5) work

**Status: the A5 rewrite is sound** — it is in the green suite and passed a 3-band
independent audit. The two-field split behaviour is correct and now asserted. No
correction needed. Two non-blocking follow-ups to fold in (your call on scope):

1. **Coverage gap (minor):** the round-trip test only exercises `NativeWavPackApe`
   (`numbering.wv`); there is no `LoftyApe` round-trip caller of
   `assert_ape_numbering_backend_round_trip` (`src/tui/probe.rs:12976`). `APE_NUMERIC`
   is declared for both backends (`src/metadata_persistence.rs`), so a `LoftyApe`
   fixture round-trip would close the parity gap.
2. **External-dependency panic (ties to Item 7):** replaying the *cumulative* numbering
   sequence (writing invalid fraction-into-total values in succession) drives a
   `str`-slice panic. Backtrace shows it is **inside lofty**, not tonepoet:
   `lofty::id3::v1::write::encode::resize_string` doing `split_at` on a non-char boundary
   (ID3v1's 30-byte field truncating a **multibyte** value), reached via tonepoet's
   lofty write-**fallback** path (`save_prepared_lofty_tags`, `src/tui/probe.rs:10124`).
   tonepoet's own numbering code is slice-safe (`split_once` throughout). Not reachable
   from clean input or the normal UI; but real-world multibyte tags + the lofty fallback
   could hit it. A guard belongs with Item 7's repair work (char-boundary-safe
   pre-truncation before the lofty save, or skip the ID3v1 write for WavPack).

---

### Item 2 — Cut/paste directory move fails; add deterministic move undo/redo; add text-field undo/redo

**2a. The move bug (empirical).** User highlights a folder, `Ctrl+X` (cut), navigates to
a new path, `Ctrl+P` (paste), and gets:

```
copy refused because /home/daedalus/library/various/VA - A Riot in Blues (1990) [FLAC] {MFSL}
cannot atomically publish a new directory without replacement; no undoable destination was created.
```

*Current behaviour (verified at the syscall level):* the paste stages the directory, then
publishes it via `tui_file_picker::rename_path_no_replace()`
(`crates/tui-file-picker/src/source_guard.rs:5722`), which on Linux calls
`renameat2(…, RENAME_NOREPLACE)` (macOS: `renameatx_np(RENAME_EXCL)`) in
`rename_between_open_directories_no_replace`. When that syscall fails with
`ENOSYS`/`EINVAL`/`EOPNOTSUPP` it is mapped to `ErrorKind::Unsupported` — i.e. the
**target filesystem or kernel does not support the atomic no-replace rename flag** (common
on some overlay/FUSE/network mounts). The handler (`src/tui/keybindings.rs:34153–34157`)
then aborts the whole move with the message above. (`EEXIST` — destination already exists
— is a *separate* `AlreadyExists` branch.) So the root cause is **not** a missing
"replacement": it is the absence of a *fallback* when the atomic no-replace primitive is
unavailable. The message's "without replacement / no undoable destination" wording merely
describes that nothing was published, not the cause.

*Clipboard/dispatch anchors:* `handle_browse_filesystem_clipboard_key`
(`src/tui/keybindings.rs:5702`) maps `Ctrl+X`→`TreeCut`/`CutSelection`,
`Ctrl+P`/`Ctrl+V`→`TreePaste`/`PasteSelection`; dispatched in
`src/tui/context_menu.rs` (`TreeCut` ~2348, `TreePaste` ~2368, `CutSelection` ~2478).

*What should happen:* when `renameat2(RENAME_NOREPLACE)` reports `Unsupported` on the
target filesystem, degrade gracefully instead of aborting the move — confirm the
destination does not already exist, then publish via a supported non-clobbering path (a
plain `renameat`/`rename`, or the existing copy+fsync+verify staging path), so a normal
folder move to a fresh destination succeeds. This is a graceful-degradation fix (per the
rigor-vs-usability directive: the default path degrades gracefully), independent of the
undo mechanism in 2b.

**2b. Move undo/redo — the user's explicit design.** There is already a
`FileOperationUndoJournal` (`src/tui/app.rs:10733–10858`) with `Copy`/`Move`/`Rename`
kinds and `Ctrl+Z`/`Ctrl+Y` bindings (`src/tui/keybindings.rs:2475`, `2494`); the
directory-move failure in 2a aborts before any entry can be journaled. The user
does **not** want elaborate rollback infrastructure. The rule:

> Undo/redo a move by **replaying the last move command in reverse** (move it back).
> This must be **deterministic**: if the user performs any mutable/destructive operation
> on the moved file/directory *before* undoing/redoing, the last move is **invalidated**
> and undo/redo is a **no-op** (do nothing).

So: record `(src, dst)` + an invalidation guard (e.g. the moved node's identity/mtime at
move time); undo = move `dst`→`src` iff the guard still matches; redo = move `src`→`dst`
under the same guard; any intervening mutation to the moved item clears the entry.

**2c. Text-field undo/redo is missing everywhere (empirical).** Inline text editors use
`TextInputState` re-exported from `tui-file-picker` (`src/tui/text_input.rs:3`) and have
**no** undo/redo history. Inline-edit entry points that should gain per-field undo/redo:
- convert-screen metadata edit — `begin_convert_metadata_inline_edit`
  (`src/tui/keybindings.rs:3029`), key handler `:3137`
- convert-screen output options — `begin_output_options_inline_edit` (`:3282`), handler `:3348`
- browse **info-pane** metadata edit — `begin_browse_metadata_inline_edit` (`:3451`), handler `:3635` (this is the "info pane" editor per `src/tui/app.rs:11145`; render at `src/tui/draw_browse.rs:2893`)
- browse rename — `begin_browse_inline_rename` (`:3414`)
- browse create — `begin_browse_inline_create` (~`:1095`)
- the metadata-editing overlay's field editors (Items 4–6 area)

*What should happen:* add an undo/redo history stack to the shared text-input state (the
`tui-file-picker` `TextInputState` is the single choke point, so all sites benefit at
once) with the conventional bindings inside a live field. Respect the byobu-safe input
rule (no F-keys; don't make Ctrl+Z the only path if it collides with anything).

*Relevant files:* `crates/tui-file-picker/src/{source_guard.rs,text_input.rs}`,
`src/tui/{keybindings.rs,context_menu.rs,app.rs}`.

---

### Item 3 — Surface integer vs float sample format (32i/32f, 64i/64f) for the source

**Empirical:** a WavPack file encoded at 32-bit **integer** displayed identically to a
32-bit **float** file; the user had to open foobar2000 to tell them apart.

*Current behaviour:* the TUI probe **captures bit depth but discards the sample-format
type.** `SourceInfo` (`src/tui/probe.rs:8–17`) has `bit_depth: Option<u32>` and no
sample-format field. `probe_audio` (`src/tui/probe.rs:349–490`) reads `audio.format()`
(`:433`) but keeps only `fmt.bytes()` — the int/float identity is thrown away.
`codec_display` (`src/tui/probe.rs:222–233`) renders `"{codec} {depth}-bit"`, so WavPack
32i and 32f both print `WavPack 32-bit`. Only literal `pcm_f32*/pcm_f64*` codec names get
a "PCM Float" label (`:222`); WavPack's `sample_fmt` is never inspected. Displayed in the
source pane at `src/tui/draw_source.rs:362–381`.

*The distinction already exists in the pipeline — reuse it.* `classify_source_audio_probe`
(`src/convert/pipeline/types.rs:1832–1918`) consumes a `sample_fmt` string and encodes
float as sentinel depths (32f→`320`, 64f→`640`, incl. `codec == "wavpack" && sample_fmt
starts_with "flt"/"dbl"`). The pipeline materializers already pass `sample_fmt` from
ffprobe (`src/convert/pipeline/materializer_single.rs:209`, `:268`). The TUI probe is the
only path that drops it.

*What should happen:* capture the sample-format type in the TUI probe (ffmpeg-next
`audio.format()` distinguishes `S16/S32/FLT/DBL` and planar variants; or read
`sample_fmt` the way the materializer does) and surface int vs float in the source
display — e.g. `32-bit int` vs `32-bit float`. `classify_source_audio_probe` already
encodes this distinction, so it's a natural seam if you agree it's the right one.
64-bit integer effectively never occurs and ffprobe may not represent
it; focusing on 32i vs 32f (and 32f vs 64f) is sufficient — if 64i is unrepresentable,
say so rather than inventing it. The target-side already models this
(`BitDepthChoice::{Int32,Float32,Float64}`, `src/tui/app.rs:275`); this closes the source
side.

*Relevant files:* `src/tui/probe.rs`, `src/tui/draw_source.rs`,
`src/convert/pipeline/types.rs` (classifier to reuse).

---

### Item 4 — "Remove all tags" (context menu + tags-button popup, with confirmation)

*Current behaviour:* the browse context menu's **Tags & Tagging** submenu is built by
`build_tagging_submenu()` (`src/tui/context_menu.rs:623`, block 611–738); it already uses a
`separator()` helper and hosts Edit metadata / Get tags / Copy tags / Transfer tags. The
metadata-editing overlay's **tags** button (footer pill, `src/tui/draw_overlays.rs:5349`)
opens `build_metadata_tags_popup()` (`src/tui/context_menu.rs:746–796`) — currently three
submenus, no separators. A confirmation-dialog mechanism exists:
`draw_confirmation()` (`src/tui/draw_overlays.rs:1391–1478`) driven by `ConfirmAction`
(`src/tui/app.rs:10461`). New actions are `ContextAction` variants
(`src/tui/context_menu.rs:97+`) dispatched in `execute_context_action`.

*What should happen:*
- Context menu: add **Remove all tags** as the **bottom** entry of Tags & Tagging, with a
  `separator()` above it, wired to a yes/no confirmation before it acts.
- Tags-button popup: add **Remove all tags** as the **bottom** entry, likewise gated by a
  yes/no confirmation.
- "Remove all tags" = strip the entire tag payload from the target file(s) (all carriers),
  using the existing atomic write paths. Confirm scope semantics with the existing
  Canonical/All precedent (Item 6) — this is *all* tags, not just canonical.

*Relevant files:* `src/tui/context_menu.rs`, `src/tui/draw_overlays.rs`, `src/tui/app.rs`.

---

### Item 5 — Make the metadata-editing overlay maximizable to the full terminal

*Current behaviour:* the overlay is fixed at ~85% of the terminal
(`metadata_editor_layout_for_area`, `src/tui/draw_overlays.rs:4842–4889`, returning a
`MetadataEditorLayout`). The convert-screen panes and browse/info panes already have a
maximize/collapse UX to mirror: `ConvertLayout::{Default, Maximized(pane)}`
(`src/tui/app.rs:254`), `toggle_maximize` (`src/tui/app.rs:5311`), unicode triangles
`▸` (collapsed) / `▾` (maximized) in the title bar (e.g. `src/tui/draw_source.rs:253–281`),
double-click via `TuiButton::Pane(focus)` (double-click detection + `toggle_maximize`
at `src/tui/keybindings.rs:41089–41107`) and an explicit `TuiButton::MaximizeToggle`
button (`src/tui/keybindings.rs:41108`, variant at `src/tui/button_map.rs:101`). Note:
there is no `TuiButton::PaneTitle` variant — pane clicks come through `TuiButton::Pane`.

*What should happen:* give the overlay the same affordances — a `▸`/`▾` indicator in its
title block (`src/tui/draw_overlays.rs:~4908`) and double-click-on-title-bar to toggle
between the current 85% size and full terminal area. The convert-pane maximize/collapse
pattern is the obvious model to follow; the exact state and plumbing are your call.
Respect the byobu-safe input rule (double-click is fine; no F-keys).

*Relevant files:* `src/tui/draw_overlays.rs`, `src/tui/app.rs`, `src/tui/keybindings.rs`,
`src/tui/button_map.rs`.

---

### Item 6 — "View" selector in the overlay: canonical vs all embedded tags

*Empirical:* the overlay surfaces a curated tag set that "seems just right," but there is
no way to see *all* embedded tags — which matters for diagnosing odd/invalid fields
(Item 7).

*Current behaviour:* the convert-screen canonical field set is `ConvertMetadataField`
(6 fields: Title/Artist/Album/AlbumArtist/Genre/Year, `src/tui/app.rs:4960–4977`); the
overlay's editing tab renders a tag-entry list (`src/tui/draw_overlays.rs:~5296+`). A
canonical/all distinction already exists elsewhere in the tag machinery
(`TagTransferScope::{Canonical,All}`, `TagCopySelection::{All,CanonicalOnly}`,
`src/tui/context_menu.rs`), so there is precedent for the two-mode concept.

*What should happen:* add a **View** selector to the overlay toggling between:
- **Canonical** — the current curated set (leave as-is; the user likes it), and
- **All** — every displayable embedded tag on the file (read the full tag payload, incl.
  non-canonical/custom keys; this is where the user would see and then repair or delete a
  stray field).

When **All** is selected, maximize the overlay (reuse Item 5) so the full list fits. Note:
confirm what the overlay's current editing tab already shows vs the 6-field convert set —
they may differ; the goal is a deliberate Canonical/All toggle, not a coincidental one.

*Relevant files:* `src/tui/draw_overlays.rs`, `src/tui/app.rs`, `src/tui/probe.rs`
(full-tag read).

---

### Item 7 — "Repair tags" (generalize existing invalid-key removal + strip FLAC ID3 prefixes)

*Important — this is partly built already; the item is surface + generalize + extend, not
build-from-scratch.* The user's premise ("tonepoet doesn't offer a way to fix invalid
tags") is only partly true today:

**What already exists (invalid APEv2 keys, e.g. cyrillic `&год`):**
- Detection: `native_ape_error_is_eligible`, `ape_key_is_valid`, `display_escaped_ape_key`
  (`src/metadata_persistence.rs:116–152`); fallback read
  `read_native_ape_fallback` (`:489`) surfaces a warning listing the escaped invalid keys.
- Repair (writable): `remove_invalid_ape_items_atomic` (`src/tui/probe.rs:9118–9237`) drops
  invalid items atomically with verification, via `prepare_native_ape_replacement`'s
  `drop_invalid_items` flag (`src/tui/probe.rs:8834`).
- UI (limited/undiscoverable): a **"Remove invalid APE key(s)"** menu item
  (`src/tui/keybindings.rs:26430`) → `ContextAction::MetadataRemoveInvalidApeKeys`
  (`src/tui/context_menu.rs:238`, dispatch `:2874`, with a confirmation) — but it appears
  **only in the metadata-editor row context menu, and only when a warning is present.**
  This is likely why the user believed there was no fix: they can't see the field, and the
  action isn't surfaced where they looked.

**What does NOT exist yet (FLAC with ID3v2 prefix):**
- Detection exists and is reusable/read-only: `detect_flac_stream_offset`
  (`src/metadata_persistence.rs:523–586`) validates a legacy ID3v2 block prepended before
  the `fLaC` marker (the 2006–2009 EAC-rip class — see the `backlog-flac-id3-prefix-tooling`
  history). But current writes **preserve** the prefix (`stream_rewrite`,
  `src/tui/probe.rs:2189–2277`, copies it). There is no "strip the prefix" repair.

*What should happen:* a single, discoverable **Repair tags** action, exposed in **both**
the context-menu **Tags & Tagging** submenu and the overlay **tags** button popup, that:
- if the file is clean → **no-op** (report "nothing to repair");
- if it has invalid APEv2 keys → run the existing `remove_invalid_ape_items_atomic` path;
- if it is an ID3v2-prefixed FLAC → strip the prefix by rewriting the stream from the
  `fLaC` magic (adapt `stream_rewrite`/the native temp+rename path; the detection helper is
  ready), preserving the healthy stream + real metadata;
- also add the char-boundary-safe guard from Item 1.2 so the lofty ID3v1 truncation panic
  can't fire during a repair write.
- a confirmation prompt before mutating (reuse `ConfirmAction`).

This dovetails with the deferred **Utilities-menu scanner** in the
`backlog-flac-id3-prefix-tooling` history (a read-only scan-at-scale that optionally
offers to fix); Repair tags is the single-file/album counterpart. `build_utilities_submenu`
(`src/tui/context_menu.rs:826`, block 810–869) is the natural home for a future scanner.

*Relevant files:* `src/metadata_persistence.rs`, `src/tui/probe.rs`,
`src/tui/context_menu.rs`, `src/tui/keybindings.rs`, `src/tui/draw_overlays.rs`.

---

### Item 8 — foobar2000 "Optimize file layout": what it is, and how it relates to Item 7

**What it is (researched):** foobar2000's context-menu **Utilities → Optimize file layout**
does *not* validate or repair tags. It rewrites the file's internal byte layout:
removes/minimizes metadata **padding** and moves the tag/album-art blocks to the **front**
of the file (faster metadata + art loading, esp. on portable players; recovers wasted
space). Two variants: **Optimize file layout** (rearrange, keep some padding) and
**Optimize file layout + minimize file size** (strip padding entirely — smaller file, but
future tag edits are slower because there's no padding to write into).

**Relation to Item 7:** it is a *separate concern* from repairing invalid/corrupt tags —
layout/padding hygiene, not correctness. It should **not** be folded into "Repair tags"
(mixing "fix broken tags" with "compact padding" would surprise users). If desired, it
belongs as its own **Utilities** entry (e.g. "Optimize tag layout" / "Compact padding"),
reusing the same atomic stream-rewrite machinery Item 7 uses. tonepoet's FLAC padding is
already handled during metadata writes (the padding/overflow path around
`stream_rewrite`), so a foobar-style "minimize padding" is a small, well-scoped utility if
the user wants it — but it is optional and out of Item 7's scope.

*Recommendation:* keep Item 7 = correctness (invalid keys, ID3-prefix strip, no-op if
clean); treat "optimize/compact layout" as an independent optional Utilities item.

Sources:
- foobar2000 changelog / feature docs (Optimize file layout): https://www.foobar2000.org/changelog-old
- fooyin issue mirroring the feature description: https://github.com/fooyin/fooyin/issues/432

---

## Suggested sequencing (non-binding)

Independent, so any order. If bundling: Items 4–6 share the overlay/menu surface and could
land together; Item 7 builds on the same menu surface + existing repair path; Item 2 is
self-contained (browse/file-picker + text-input); Item 3 is a contained probe/display
change; Item 1's follow-ups are small and can ride with Item 7. Every new/changed
behaviour needs a pin, and the full-suite baseline (5384/0) must stay green.
