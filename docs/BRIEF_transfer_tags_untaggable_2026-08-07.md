# tonepoet — Transfer Tags for untaggable carriers (both directions) (2026-08-07)

You are starting **fresh**; everything you need is in this bundle. Outcomes + guardrails;
diagnosis is evidence, not prescription — you choose HOW.

**Project:** tonepoet (Rust TUI, ratatui 0.26 / crossterm 0.27, tokio, edition 2021),
version 0.4.6 — do not bump. Gate `cargo test --workspace --no-fail-fast` green ×2.

## The problem

The TUI **Transfer Tags** feature does not work for folders of **untaggable carriers**
(`.dff`, and by the same mechanism `.shn`/`.dts`/`.ac3`) that have a sidecar `.cue`. It
fails in **both directions**:
- **Transferring FROM** such an album (using it as the source) → error
  `tag transfer source read failed for '…/01 - ….dff' (9 of 9 sources unreadable): …`.
- **Transferring TO** such an album (using it as the target/destination) → the write cannot
  land tags on the untaggable carriers.

Reproduction: right-click `~/torrents/Michael Jackson - Thriller. 1984 Japan/` → Properties
→ Tags → Transfer tags → Canonical → pick a source → the read error above. The user reports
neither "transfer to a folder/file(s)" nor "transfer from a folder/file(s)" works for these.

The metadata EDITOR (Properties) already treats a valid sidecar cue as the authority for
untaggable carriers (`metadata_editor_untaggable_sidecar_authority`, keybindings.rs ~10172,
plus the shipped cue-writeback that saves edits to the sidecar). Transfer Tags has **no**
equivalent — it insists on reading/writing the carriers directly.

## Diagnosis (consensus-verified by two independent audits)

### Read side (transfer FROM — confirmed)
For a `TransferCarrier::SidecarCue` whose role is `MetadataSidecar`, the transfer path
extracts the cue entries (src/tui/tag_interchange.rs ~3038, `cue_sheet_transfer_entries`) but
then STILL reads the individual carriers:
```
SplitCueMemberRole::MetadataSidecar => {
    let file_entries = read_transfer_source_entries(track_audio_paths, scope, cancel)?;  // ~3061
    overlay_cue_entries_on_file_entries(file_entries, cue_entries, sheet.tracks.len())
}
```
`read_transfer_source_entries` (~3131) reads every carrier via lofty; `.dff` →
`UnknownFormat` → `MetadataReadIssueKind::UnsupportedFormat` (probe.rs ~7912) →
`blocks_metadata_use()` true (probe.rs ~7905, true for anything but a
`RecoverableTagWarning`) → the whole op aborts with the "(N of M sources unreadable)" error
(tag_interchange.rs ~3154). The `?` makes one unreadable carrier fail everything. The
**sibling `EmbeddedCue` branch (~3085-3098) shows the correct pattern**: it derives entries
from `cue_sheet_transfer_entries(sheet)` without demanding a readable carrier.

### Write side (transfer TO — confirm and complete)
The corresponding write/apply path for a `MetadataSidecar` untaggable target must route tags
to the sidecar cue (as the editor's cue-writeback does), not attempt to write embedded tags
to `.dff`. Trace the transfer WRITE path (the apply/commit half of Transfer Tags —
tag_interchange.rs + the keybindings/context_menu dispatch that commits transferred entries)
and confirm where an untaggable target currently fails or no-ops, then route it through the
established sidecar-cue writer.

## Outcomes

**T1 — Transfer FROM an untaggable+cue album works.** Using such an album as the transfer
SOURCE reads its metadata from the sidecar cue (authority), tolerating unreadable carriers
instead of aborting. The cue's per-track TITLE/ARTIST/ISRC and album fields become the
source entries. An unreadable untaggable carrier is NOT a fatal error when a valid cue
supplies the metadata (mirror `metadata_editor_untaggable_sidecar_authority`). If NO valid
cue exists, the existing honest failure is fine.

**T2 — Transfer TO an untaggable+cue album works.** Using such an album as the transfer
TARGET writes the transferred canonical tags to the **sidecar cue** via the established
atomic sidecar-cue writer (the same one the metadata editor's untaggable-carrier save uses),
with per-carrier embedded-tag writes marked Blocked/Unsupported — never a hard failure, never
a false "wrote tags to .dff" claim. Honest status/log about where the tags landed (the cue).

**T3 — Both scopes, both selection shapes.** Works for the transfer SCOPES (Canonical / All /
field subsets) and for folder AND file(s) selections, consistent with taggable behavior.

**T4 — Consistency with the editor.** The values a track gets via Transfer Tags for a given
cue album match what the metadata editor shows/writes for the same album — one cue-authority
principle across editor and transfer. Reuse the editor's untaggable-authority + cue-writeback
machinery; do not invent a second cue reader or writer.

## Guardrails
- Do NOT regress taggable-album Transfer Tags (FLAC etc. read/write carriers directly as
  today), the `EmbeddedCue` and split-source paths, or non-cue folders.
- Untaggable class defined by the existing classification (lofty-unsupported /
  `blocks_metadata_use`), not an extension list — DFF/SHN/DTS/AC3 and whatever else lofty
  can't tag.
- Lodestar-governed (docs/metadata_source_selection_heuristic.md, bundled) — source
  selection/admission unchanged; full gate ×2.
- Byobu-safe input rules; no new deps; version 0.4.6. Established sidecar-cue writer is the
  only writer.
- Tests (drive the real Transfer dispatch, both directions): (a) transfer FROM a dff+cue
  album yields cue-sourced entries, no abort on unreadable carriers; (b) transfer TO a
  dff+cue album lands canonical tags in the sidecar cue, carriers untouched, honest status;
  (c) a non-DFF untaggable variant (SHN/DTS) both directions; (d) regression: taggable
  album transfer unchanged; (e) no-valid-cue untaggable album still fails honestly.

## Deliverables
Complete replacement files or unambiguous patches; a WHY summary (how read tolerates
unreadable carriers under cue authority; how write routes to the sidecar; reuse of the
editor's machinery); test list; honest unverifiable-in-your-environment note (no real .dff
fixtures unless you synthesize headers — cue parsing/mapping is testable without DSD audio).

## Bundle manifest
- This brief; docs/metadata_source_selection_heuristic.md (LODESTAR).
- Complete `src/` tree (esp. src/tui/tag_interchange.rs, keybindings.rs, context_menu.rs,
  probe.rs, and the editor's untaggable-authority + cue-writeback code for reuse) +
  `crates/tui-file-picker`; root `Cargo.toml`, `CLAUDE.md`.
NOT included: other workspace crates, target/, other docs. If anything is missing, say so
rather than guessing.
