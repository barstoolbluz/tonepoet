# Browse/Editor Round-5 Brief — six field items

**Branch:** `hardening` @ c12da89. **Baseline suite:** 5,188 passed / 0 failed
(56 targets). **Version stays 0.4.4.**

All mechanisms below are research-verified with citations; do not re-derive
them. The [rigor-vs-usability directive](perf-restoration-brief.md) (§0 there)
remains standing: default paths favor casual usability; fail-closed refusals
on common real-world material are defects.

## Item 1 — lowercase `go` on the path row

The path row renders `" Go "` (src/tui/draw_browse.rs:412; `GO_WIDTH: 5`
unaffected). The file picker's address bar uses `go_label = "Go"`
(crates/tui-file-picker/src/render.rs:293). Lowercase both to `go` for
parity with the surrounding lowercase button labels. Cosmetic; keep hit
rects unchanged.

## Item 2 — metadata-editor scrolling: Add-field row unreachable / invisible input

Field symptom: with many fields, the `+ Add field...` row can never be
scrolled into view; right-click → Add field works but the input row (and
cursor) is off-screen while typing.

Mechanism (verified): the Add-field row IS inside the scroll region
(draw_overlays.rs:4957 `total_rows = entries.len() + 1`, window applied at
:5296). The defect is the scroll setter: `ensure_cursor_visible`
(keybindings.rs:26754-26763) recomputes visible rows with an ad-hoc
`terminal::size()` formula that subtracts 4, while the real layout
(`metadata_editor_layout_for_area`, draw_overlays.rs:4819-4866) consumes 5
rows (2 border + 2 tabs + 1 footer) and also clamps popup height. The
estimate is one row too large (worse on short terminals), so the cursor row
— including the Add-field row at index `entries.len()` — always sits exactly
one row below the painted window. Every scroll gesture on the Metadata tab
funnels through this setter (wheel keybindings.rs:25966-25977; keyboard
:12912-12932) — the last row can never paint, keyboard or mouse.

Additionally the context-menu path (`ContextAction::MetadataAddField`,
context_menu.rs:1745-1751) hand-sets phase/input without touching cursor or
scroll at all, bypassing the guards and ensure-visible that
`metadata_editor_open_add` (keybindings.rs:9257-9265) provides.

Required fix shape:
1. `ensure_cursor_visible` derives visible rows from
   `metadata_editor_layout_for_area(...)` content height — the pattern
   `metadata_editor_read_only_visible_rows` (keybindings.rs:17743-17747)
   already follows — instead of the −4 guess.
2. `ContextAction::MetadataAddField` routes through
   `metadata_editor_open_add` (shares the writable-slot guard, cursor
   placement, and ensure-visible).
3. Same-pass check of the sibling `ensure_detail_visible`
   (keybindings.rs:26766+) which uses the same ad-hoc formula (−6).
4. Tests: none currently pin field-list scrolling. Add pins: bottom-row
   (Add-field) cursor lands inside the painted window at representative
   heights; the context-menu add path scrolls the input row into view.

## Item 3 — FLAC tag write refused on ID3v2-prefixed files

Field symptom: `native FLAC metadata-region tag write refused for <path>` on
`~/library/elton/Elton John - 17-11-70 (1971) [FLAC] {Japan SHM}` (note:
single space in `{Japan SHM}` on disk).

Forensics (verified on the real files): all 7 tracks carry an **ID3v2.3 tag
prepended before the `fLaC` marker** — sizes VARY per track (4,619-4,720
bytes; track 1 = 4,643 = syncsafe 4,633 + 10 header, `fLaC` at 4643
byte-exact, unsynchronization flag 0x80). **A constant offset must not be
assumed; per-file prefix parsing is mandatory.** Vendor string "reference
libFLAC 1.2.1 20070917" — classic 2007-era EAC rip. The streams are healthy: normal
STREAMINFO/SEEKTABLE/VORBIS_COMMENT chain plus **8,056 bytes of padding** —
in-place writable once the offset is honored. `metaflac` reads them fine.

Mechanism: `.flac` extension → `MetadataPersistenceRoute::NativeFlacVorbis`
(src/metadata_persistence.rs:157-166) → native writer →
`read_flac_metadata` demands `fLaC` at byte 0 (probe.rs, "is not a FLAC
stream") → wrapped into `native_flac_write_refused_error` (probe.rs:8065).
Display reads work because they go through lofty, which skips ID3 prefixes —
so the user sees tags they cannot edit.

Required fix shape: teach the native FLAC machinery a **bounded ID3v2-prefix
skip**: detect `ID3` magic at byte 0, parse the syncsafe size (+ footer bit
ONLY for version 2.4 — flag 0x10; these files are v2.3, key it off the
version byte), require `fLaC` immediately after, and thread the region base
offset through: the reader; the in-place region overwrite — NOTE it has TWO
hard-coded-offset sites, its own fLaC re-verification at probe.rs:1979-1984
("FLAC magic changed") AND the seek to byte 4 at :1985; the overflow
`stream_rewrite` (probe.rs:1998 — today it writes FLAC_MAGIC at output byte
0 and would silently DROP the prefix; see decision below); the recovery
restore paths (`restore_metadata_snapshot*`, probe.rs:1244-1296, hard-code
`4 +` length checks); and the metadata journal (record the offset so
recovery restores at the right position — NOTE this changes the journal
binary format: recovery MUST still parse pre-existing offset-less journals;
version the format or default the missing field to 0). Bounds: reject
prefixes above a sane cap (e.g. 16 MiB) and malformed syncsafe sizes (any
byte with bit 7 set) — fail closed only on genuinely unparseable prefixes,
with the existing refusal message.

Routing consistency (audit-found): `has_flac_magic`
(metadata_persistence.rs:144-150) reads only bytes 0-3, so an ID3-prefixed
FLAC with a NON-.flac extension routes to Lofty today. The reusable
prefix-detection helper should also back `has_flac_magic` so magic-based
routing agrees with the writer. Display nuance worth knowing: lofty PARSES
the ID3 prefix (lofty flac/read.rs finds and reads it) — displayed values
may merge ID3 and Vorbis tags when they disagree; the native writer edits
only the Vorbis comments, which is correct and unchanged. DECISION for the model to take and document: on overflow
rewrite, preserving the ID3 prefix is the conservative default; stripping it
is arguably a repair (the prefix is redundant legacy baggage) but changes
bytes the user didn't ask to change — recommend preserve-by-default, and
surface an explicit repair affordance only if trivially cheap. All existing
guards (symlink/hardlink/journal) unchanged. Tests: fixture builder that
prepends a real ID3v2.3 header (with unsync flag) to an existing FLAC
fixture; pin read, in-place write within padding, overflow rewrite, journal
recovery at offset, and the malformed-prefix refusal.

SCOPE FENCE: this round fixes the WRITE PATH ONLY (tag edits work on these
files). A library-scale scanner in the Utilities menu (find all
ID3-prefixed/problem FLACs, optional batch fix) and a dedicated repair tool
(silent repackage, possibly via the convert pipeline machinery) are a
DEDICATED FUTURE ROUND — do not build them now, but do not preclude them:
the prefix-detection helper should be reusable (clean function, not inlined
into the writer).

## Item 4 — `audio streams` button broken at folder level for ISOs

Two distinct sub-issues, both diagnosed:

**(A) Folder-level click always fails.** The pill's visibility for a
highlighted Directory resolves the folder classification to the **nested ISO
inside** (`FolderClassificationKind::Disc` →
`classification.disc_probe_source_path(entry_path)`, draw_browse.rs:3260,
browse.rs:1085-1094) and even schedules the disc probe against it
(browse.rs:6915-6927). The click handler, however
(`open_selected_disc_browser` → `selected_entry_effective_disc_path`,
disc_browser_actions.rs:56-96), only accepts an entry whose OWN kind
`is_disc_source()` or an `.iso` archive — a Directory fails both → "Selected
entry is not a browsable disc source" (disc_browser_actions.rs:61; note the
existing string spells "browsable"). Show-logic and click-logic have
disagreed since `0e4f874` introduced folder classification without touching
the handler; folders that ARE disc roots (BDMV/DVD-A dir structures — entry
kind is disc-source) always worked, which is the remembered behavior.

Required fix: add the folder-classification arm to
`selected_entry_effective_disc_path` — the union predicate already exists as
`current_selected_disc_source_matches()` (browse.rs:6807-6820): entry is a
disc source OR its valid folder classification is `Disc`, in which case the
effective path is `disc_probe_source_path`. Path-key alignment is verified:
the folder probe already caches under the ISO path, and Convert works from
`DiscContents.source_path` (the probed ISO), so downstream consumers convert
the ISO, not the folder. Scope precision (audit-verified): the same helper
serves `ConvertDiscDefault` (disc_browser_actions.rs:213-224) and
`BrowseInfoAnalyze` (:289-296), and the fix makes it CORRECT for all three —
but only the audio-streams pill is REACHABLE from a Directory entry today
(the Directory context menu has no disc entries, and no analyze pill renders
in the folder arm). Do NOT add folder-level menu entries or pills for
Convert-default/Analyze this round. One-line note: `ConvertDiscStream(id)`
(disc_browser_actions.rs:~248) bypasses the helper and uses the raw entry
path — unreachable from folders today; leave it, but do not route new
folder-level UI through it. Tests: folder-containing-ISO fixtures for all
four disc types (DVD-A/DVD-V/SACD/BD ISO) pin both the pill visibility AND
successful activation from the folder level.

**(B) re-ac-tor shows no button at all — NOT encryption.** Empirically
probed the real ISO (read-only): `disc-info` reports "Copy protection: MKB
present, AOBs readable" — the CPPM refinement
(crates/dvda-demuxer/src/tui/dvda/cppm.rs:38-99, mapped at
src/disc/dvda_mapper.rs:184-207) correctly clears the encryption flag, and
encryption never gated the pill anyway (it only adds an info line,
draw_browse.rs:3129-3139). The actual gate: the pill requires
`contents.presentations.len() >= 2` (draw_browse.rs:3154). Re-ac-tor is a
single-group stereo DVD-A (1 presentation, 8 tracks) → suppressed — and by
the same predicate it's suppressed even when the ISO is highlighted
directly. DECISION taken by this brief: **lower the gate to ≥ 1** — the
overlay has per-track value (track listing, single-stream confirmation)
beyond stream choice, and a button that appears for some discs but not
others of the same type reads as breakage (this exact field report).
Downstream safety verified: `open_disc_browser_from_contents` rejects only
EMPTY presentations (disc_browser_actions.rs:119-123), and the
single-presentation overlay is already exercised today via the disc-kind
context menu's unconditional "Browse Audio Streams..." entry
(context_menu.rs:779/806) — the path is proven, not novel. Pin with a
single-presentation fixture.

## Item 5 — byobu-safe select-all: add Alt+L synonym to the text engine

Inventory (verified): the shared text engine currently binds **Alt+A →
select_all_text** for multiplexers
(crates/tui-file-picker/src/text_input.rs:796-805, regression test :1642),
and every tonepoet text surface routes through it. But the metadata editor
intercepts Alt+A as **Apply** — BY DESIGN, user-confirmed, keep it
(`metadata_editor_handle_alt_commit_action`, keybindings.rs:12669-12700,
dispatched before phase handling at :12712-12714) — so mid-edit Alt+A never
reaches the text input there, and the user does not use Alt+A for
select-all anywhere. Ctrl+Shift+A is indistinguishable from Ctrl+A in
legacy encoding (engine matches case-insensitively, text_input.rs:798) — it
can never work. Browse file list Alt+A toggle-select-all
(keybindings.rs:5923) and Queue Alt+A select-all (:6709) are list-surface
bindings for a different function.

Required fix (user-directed, no duplicate chords): **REPLACE** the shared
text engine's Alt+A select-all arm — **PRECISELY text_input.rs:802-805
ONLY** (the Ctrl+A arm at :798-801 is adjacent and MUST remain; do not
replace the wider 796-805 span) — with **Alt+L**, verified collision-free
across the entire codebase twice independently (zero Alt+L bindings
anywhere; no eq_ignore_ascii_case('l'); no generic Alt-char absorber). The
engine keeps exactly two select-all chords: Ctrl+A (non-multiplexed
sessions) and Alt+L (byobu-safe). Alt+A is thereby freed in text contexts
entirely; in the metadata editor it remains **Apply** — by design,
footer-advertised and test-pinned (test at keybindings.rs:52214), do NOT
change it. The list-surface Alt+A bindings are a DIFFERENT function and
stay untouched (Browse toggle-select-all keybindings.rs:5923, Queue
select-all :6709). Concrete update sites: the engine regression test
`alt_a_is_a_terminal_safe_select_all_alias` (text_input.rs:1637) flips to
Alt+L; the doc comment at text_input.rs:747 ("Ctrl+A or Alt+A=select all")
and the arm comment at :796-797 are the only chord listings in the tree —
no user-visible footer lists the select-all chords. Known behavioral
nuance (accepted): inside the metadata editor's embedded file picker the
Apply interceptor stands down, so Alt+A select-all currently works there;
after the swap it becomes a no-op and Alt+L takes over — consistent, no
action needed beyond the engine swap. Tests: engine-level pin that Alt+L
selects all and Alt+A no longer does; a metadata-editor-mid-edit pin that
Alt+L selects the value text while Alt+A still applies.

## Item 6 — `Copy tags` submenu under Tags & Tagging (+ surfaced Paste)

New submenu inside `build_tagging_submenu`
(src/tui/context_menu.rs:571-606), using the existing nested-submenu +
separator machinery (three-deep nesting precedent: File operations ▸ Rename
▸ Fix capitalization, context_menu.rs:422-455; `separator()` at :331).

Menu shape (user-specified):

```
Tags & Tagging ▸
  ...existing entries...
  Copy tags ▸
    All
    Canonical Only        # all fields below except Custom
    ──────────
    Artist
    Album
    Title
    Year
    Genre
    Performer
    Composer
    Album Artist
    Track Numbers
    Comment
    ──────────
    Custom...             # SURFACED ONLY this round: plain ENABLED item
                          # emitting an honest status; the editable builder
                          # with tab-complete on tag-field names arrives in
                          # the NEXT round
  Paste tags              # SURFACED ONLY this round: plain ENABLED item
                          # (no submenu chevron yet) whose action emits an
                          # honest status; implementation NEXT round
```

Design constraints (research-verified):
- **Tag clipboard**: none exists. Introduce a session-scoped store —
  precedent: `shared_text_input_clipboard()` static Mutex
  (text_input.rs:7-11) or `BrowseState.filesystem_clipboard`
  (browse.rs:2290). Schema (audit-hardened, so next round's Paste needs NO
  change): store **full `TagEntry` clones** (NOT a slimmed struct — losing
  `row_scope` would silently misalign Track-scoped CUE-carrier rows on
  paste, and `per_file_stored_value_counts`/`is_mixed` are needed for
  broadcast/cardinality decisions) PLUS the **ordered source-path list**
  (`read_all_tags_merged_with_metadata`, probe.rs:7120, returns
  `MergedTagsAndMetadata` whose `entries: Vec<TagEntry>` are positionally
  aligned to the caller's path order — the paths are NOT inside TagEntry).
- **Field mapping** (use display keys; load-bearing footnote:
  TRACKNUMBER/TRACKTOTAL/DISCNUMBER/DISCTOTAL must match by display_key,
  NOT ItemKey equality — `canonical_editor_item_key` (probe.rs:6249-6261)
  deliberately maps the totals to `ItemKey::Unknown(...)`):
  Title=TITLE, Artist=ARTIST, Album=ALBUM, Year=DATE, Genre=GENRE,
  Performer=PERFORMER, Composer=COMPOSER, Album Artist=ALBUMARTIST,
  Track Numbers={TRACKNUMBER, TRACKTOTAL, DISCNUMBER, DISCTOTAL},
  Comment=COMMENT (alias DESCRIPTION handled by the read path).
- **Copy semantics**: operates on the context-menu target selection (single
  file, multi-selection, or folder → its audio files). Target-resolution
  precedent (audit-corrected): `current_bulk_guard_paths`
  (src/tui/command.rs:83-92, Browse selection →
  `collect_selection_for_file_ops`) and `expand_audio_paths_for_metadata`
  (:93-101, folder → contained audio incl. single-image CUE carriers) — the
  context-menu TagsFromMb dispatch itself resolves nothing. Copy reads via
  `read_all_tags_merged_with_metadata` and stores the selected field
  subset. Status line reports what was copied ("Copied 4 fields from 12
  files"); `:messages` not required for this.
- **Deferred, explicitly**: the Custom builder (tab-complete field picker)
  and Paste tags execution are NEXT round. MECHANISM (audit-forced
  decision — the current menu model makes "disabled + emits a message"
  contradictory: disabled items are skipped by navigation AND mouse
  registration (keybindings.rs:9083/27233/27359, draw_overlays.rs:696-700),
  and `Submenu` has no `enabled` field at all): surface BOTH deferred
  entries as plain ENABLED `Item`s — `Custom...` inside the Copy tags
  submenu, and `Paste tags` as a plain item (no ▸ this round) directly in
  Tags & Tagging — whose action emits ONLY the honest status message
  ("Custom tag selection arrives in a later round" / "Paste tags arrives in
  a later round") and changes nothing. Next round Paste becomes a real
  submenu. Do NOT extend the menu model's disabled semantics this round.

## Constraints (standing)

- NO function keys; NO emoji/decorative unicode; Ctrl+Q stays quit; version
  stays 0.4.4; `cargo test --workspace` green (baseline 5,188/0); never
  truncate gate output.
- Verification split (c12da89) is load-bearing: metadata writes in item 3
  must respect standard/strong modes (the ID3-prefix skip applies to the
  NATIVE writer used by both modes; do not route these files to lofty).
- Mouse text contract, 4-state cursor matrix, `:messages`, degraded-rename
  ladder: do not regress.
- Deliverables: overlay bundle with preimage manifest; engineering report
  with named pinning tests per item, decisions taken (item 3 overflow
  policy, item 4 gate change), and any deviations with rationale.
