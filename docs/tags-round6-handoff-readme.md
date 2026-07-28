# Handoff — Tag Interchange Round 6 (bundle readme)

**Governing document:** `docs/tags-round6-brief.md`. Read it fully first.
Every mechanism is audit-verified to the line (three overlapping audit bands
plus a fresh-eyes re-verification of all amendments). Do not re-derive the
verified seams, and do not second-guess the audit-forced decisions recorded
inline — in particular: the tilde empty-value rule, the paste-classification
precedence (KNOWN-key blocks outrank cursor targeting; count-typo = honest
error, never silent degrade), Ctrl+X = mark-deleted, `selected_rows`
per-surface, chord arms AFTER the content-tab guard, traversal-order
transfer pairing, and the dirty-editor blocking confirm.

**Baseline:** branch `hardening` @ 04e127f (+ docs commits); `cargo test
--workspace` = 5,214 passed / 0 failed across 56 targets. Version stays
**0.4.4**.

## Scope, in suggested order

1. §1 field-block serializer/parser module (foundation — everything else
   consumes it; property tests first).
2. §6 Queue Ctrl+L unbind (trivial; three sites).
3. §2 Copy tags → text clipboard (+ pub clipboard API in the picker crate,
   + best-effort size-gated OSC 52).
4. §5 editor clipboard citizenship (row selection, chords, per-column
   menus, paste classification, Editing-phase bracketed-paste arm).
5. §4 the `tags` popup (bottom-anchor menu mode, MB/gnuDB/Clipboard/File
   children, editor-side Transfer entries).
6. §3 Browse Transfer tags (worker, traversal-order alignment, classified
   write seam) + Paste-stub removal.

## Non-negotiable constraints

- Key bindings SCOPED to the active screen/overlay (standing principle);
  nothing new goes global.
- NO function keys; NO emoji/decorative unicode (functional ▸ set only);
  Ctrl+Q stays quit; version stays 0.4.4; delete stays non-undoable.
- `metadata_editor_apply_detail_paste` stays UNCHANGED (DetailEdit keeps
  its lenient semantics); the new row-paste entrance pre-validates.
- The internal full-fidelity TagClipboard is untouched (future
  Paste/Custom substrate). Custom builder is NEXT round.
- gnuDB network migration OUT OF SCOPE (entry ships wired; endpoint dark).
- Artwork/pictures do not serialize or transfer (disclose).
- System-clipboard READ is impossible app-side — do not attempt OSC 52
  queries or external tools; the documented paste-gesture path is the
  design.
- Do not regress: mouse text contract, 4-state cursor matrix, `:messages`,
  degraded-rename ladder, standard/strong verification split, round-5
  ID3-prefixed-FLAC support (transfer targets may be prefixed FLACs — the
  classified seam handles them; do not route around it).

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest (SHA-256 of
  the exact base revisions received) covering every modified file.
- Engineering report: per-item named pinning tests (the brief's §7 list is
  the minimum), the three-broadcast-regimes trichotomy stated, disclosed
  limitations (OSC 52 advisory, no system-clipboard read, artwork
  excluded, gnuDB endpoint dark), pinned status wordings (block-apply
  success AND empty/invalid-clipboard failure), and any deviation from
  the brief with rationale.
- `cargo test --workspace` stays green against 5,214/0; new tests must
  FAIL if the specific behavior they pin regresses.
