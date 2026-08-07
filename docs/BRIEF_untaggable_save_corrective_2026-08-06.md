# tonepoet — Untaggable-carrier sidecar save CORRECTIVE (2026-08-06)

You are starting **fresh**; everything you need is in this bundle. Outcomes + guardrails;
diagnosis is evidence, not prescription.

**Project:** tonepoet (ratatui TUI, tokio, edition 2021), version 0.4.6 — do not bump.
Gate `cargo test --workspace --no-fail-fast` green; must stay green.

## Field history — third round on ONE user scenario. Close it for good.

Scenario: `~/torrents/Michael Jackson – Thriller. 1984 Japan` — .dff carriers + sidecar
.cue. The user edits ALBUM/DATE/GENRE in the metadata editor and saves.

1. **Round 1 (shipped):** untaggable carriers + sidecar cue became an editable
   synthetic-sheet surface with saves targeting the cue.
2. **Round 2 (shipped @ 7fc8478):** save was REFUSED with ALBUM/DATE/GENRE listed as
   "unsupported changed fields". Root cause: representability consulted
   `effective_row_scope`, whose per-file-count inference (`per_file_values.len() !=
   paths.len()` ⇒ Track) diverges on legitimate shapes — e.g. carriers in the folder that
   the cue does not reference. Fixed by making representability key-based
   (`metadata_editor_cue_sidecar_representable_entry`, keybindings.rs ~10232). Also added
   `[metadata] sidecar_save_with_warnings`.
3. **Round 3 (CURRENT DEFECTS — two symptoms, same disease):**
   (a) The save reports **"Metadata 0 saved, 1 CUE sidecar already current, unsaved
   changes remain."** — nothing written, edits stranded dirty.
   (b) Editing is BLOCKED before saving even enters the picture: attempting to edit the
   album name — or, per the user, ANY field — yields **"metadata editor: Cannot persist
   per-track ALBUM on a multi-image CUE album."** The edit-time guard
   `metadata_editor_unpersistable_per_track_reason` (keybindings.rs ~9910, consulted by
   `metadata_editor_apply_inline_value_to_writable_slots` before applying any value) still
   classifies entries via `entry.is_track_scoped(surface.paths.len())` — the exact
   per-file-count scope inference round 2 removed from SAVE-time representability. On the
   user's shape it misreads album-scoped fields as per-track and refuses the edit; note
   the message also calls the album "multi-image", evidence the surface's shape state
   diverges from the entry dimensions.
   The same user, same album, third failure.

## Diagnosis (bounded; verify and complete it)

- The completion counters come from the save summary (app.rs ~6896/6941:
  `sidecar_cue_unchanged` → "N CUE sidecar already current"). "Already current" means the
  writeback compared the (re)generated cue text against the existing sidecar and found it
  IDENTICAL — i.e. the user's ALBUM/DATE/GENRE edits never reached the regenerated text.
- `regenerate_unified_cue_album_cuesheet_for_save` (keybindings.rs ~14993) has multiple
  **silent `Ok(false)` early exits** ahead of regeneration (e.g. ~15022, and the
  no-synthetic-sheet else at ~15024), plus a hard shape error at ~15027
  (`sheet.audio_paths.len() != n_paths` → Err "unified CUE album path mapping is
  inconsistent"). The user saw NO error — so the flow exited through a silent `Ok(false)`
  path (or a sibling regenerate route selected by `regenerate_cuesheet_for_save`,
  ~15133) and the writeback then honestly compared an UNREGENERATED cue. Note the theme:
  this is the same paths-vs-sheet shape-divergence family that caused round 2 — the fix
  landed in classification but the regeneration layer still gates on fragile shape
  equalities and fails SILENT instead of failing loud.
- We do not know the album's exact shape (single-image vs one-file-per-track vs extra
  carriers unreferenced by the cue). Do not assume: make the save correct across the whole
  shape matrix.

## Outcomes

**C1 — The edits persist.** On an untaggable-carrier sidecar album, saving edited
CUE-representable fields (ALBUM/ALBUMARTIST/DATE/GENRE/CATALOGNUMBER; per-track
TITLE/ARTIST/ISRC) rewrites the sidecar cue so the new values are present in the cue TEXT
on disk, across every legitimate shape: single-image multi-track, one-file-per-track
multi-FILE, and folders with carriers the cue does not reference. After a successful save
the surface is clean (no "unsaved changes remain") and reopening shows the saved values.

**C2 — No silent no-op saves, anywhere in this path.** Every early exit in the regenerate/
writeback chain that skips persisting USER EDITS must either be correct (provably nothing
to persist) or produce an explicit, actionable error. "N saved / already current" may only
be reported when it is TRUE — if dirty representable entries exist and nothing was
written, that is a failure and must say why. Audit every `Ok(false)`/early-return in
`regenerate_unified_cue_album_cuesheet_for_save`, its sibling routes dispatched by
`regenerate_cuesheet_for_save`, and `cue_sidecar_writeback_plan_for_state` for this
property.

**C3 — Shape robustness at the root.** Wherever this pipeline equates `paths.len()`,
`sheet.audio_paths.len()`, `sheet.track_sources.len()`, or per-entry value counts, decide
deliberately what the INVARIANT is for untaggable sidecar surfaces and enforce/normalize
it at construction time (the editor builder) rather than scattering divergent guards at
EDIT time and SAVE time. Round 2's lesson generalizes: shape inference at the leaves keeps
breaking — it has now produced three distinct user-facing failures from three different
leaves (representability, edit-time persistability, cue regeneration). Establish the shape
once, then trust it everywhere.

**C3a — Edits must be POSSIBLE.** The edit-time guard
(`metadata_editor_unpersistable_per_track_reason` and any sibling gates consulted before a
value is applied) must permit editing every CUE-representable field on every legitimate
untaggable-sidecar shape. Whatever scope decision it needs must come from the established
invariant (C3), not from per-file-count inference. Guards protecting genuinely structural
rows (TRACKNUMBER positionality on multi-image albums, CUESHEET) keep their protection —
scoped to the rows they are actually about.

**C4 — End-to-end proof.** Tests must drive the REAL production paths — edit application
(`metadata_editor_apply_inline_value_to_writable_slots` through the dispatch that consults
the edit-time guard) AND save (`metadata_editor_save` level) — and assert on: the edit
being ACCEPTED, the resulting cue file TEXT (contains the edited ALBUM/DATE/GENRE), the
clean dirty-state afterwards, and the honest summary counts — for at least: (a)
single-image dff multi-track; (b) multi-FILE dff one-per-track; (c) a folder containing an
extra dff the cue does not reference; (d) an SHN or DTS variant. The existing tests that
assert plan-level or guard-level outcomes were insufficient — they passed while the user's
edit was refused and the user's save did nothing.

## Guardrails
- Preserve round-2 behavior: key-based representability, structural refusals (CUESHEET
  deletion, PERFORMER inheritance) still block, `[metadata] sidecar_save_with_warnings`
  semantics unchanged (revert-visibly + warn for genuinely unrepresentable fields).
- Taggable-carrier flows (native multi-FILE albums, metadata sidecars, plain files) must
  be behaviorally unchanged; the full regression suite guards them.
- Lodestar-governed area (docs/metadata_source_selection_heuristic.md bundled): do not
  disturb source selection/admission. Full-gate ×2 posture.
- Writeback stays on the established atomic sidecar replacement helper; no second writer.
- No new dependencies; version 0.4.6.

## Deliverables
Complete replacement files or unambiguous patches; a WHY summary that names the actual
silent exit the user hit and the shape invariant you established; test list; honest
unverifiable-in-your-environment statement.

## Bundle manifest
- This brief; docs/metadata_source_selection_heuristic.md (lodestar).
- Complete `src/` tree and `crates/tui-file-picker/`; root `Cargo.toml`, `CLAUDE.md`.
NOT included: other workspace crates, target/, other docs. If anything is missing, say so
rather than guessing.
