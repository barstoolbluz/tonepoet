# tonepoet — Clipboard end-to-end + untaggable-carrier metadata brief (2026-08-06)

You are starting **fresh** with no prior context. Everything you need is in this bundle. This
brief describes **outcomes and guardrails**; the included diagnosis is *evidence*, not
prescription — you choose HOW, so long as outcomes are met and guardrails hold.

**Project:** tonepoet, Rust CLI + TUI audio toolkit (ratatui 0.26 / crossterm 0.27, tokio,
edition 2021), version 0.4.6 — **do not bump**. Gate `cargo test --workspace --no-fail-fast`
is green (5575/0 including two new reproduction tests described below) and must stay green.

Two problem areas. Both have burned the user repeatedly; both must be closed **end-to-end**
this time, with tests at the layer that actually failed.

---

## Part A — Clipboard: copy/cut/paste must work across the entire app, including to/from the host

### User-visible failures (byobu/tmux on Linux; only `xsel` installed — no xclip, no wl-copy)
1. Highlighting text in the Browse **inline rename editor** (mouse), Ctrl+C, then paste —
   fails into the same field AND into external apps (gedit, LibreOffice Writer).
2. Context menu **"Copy path"** → cannot paste into the Browse `path:` field or external apps.
3. Metadata-editor field copying **works** (the user's contrast point).

### What we PROVED before writing this brief (do not re-litigate; build on it)
Two empirical tests now in-tree
(`keybindings.rs::file_picker_browse_parity_regression_tests::
inline_rename_copy_then_paste_round_trips_through_real_dispatch` and
`copy_path_action_pastes_into_path_input_through_real_dispatch`) drive the EXACT reported
flows through the production `handle_key` dispatch and context-action executor. **Both
pass.** Keyboard-model copy publishes to the shared text clipboard, mirrors once to the
host hook, and Ctrl+V pastes back. Therefore the defects are NOT in key dispatch. The
remaining suspect layers, in order of likelihood:

1. **Host clipboard transport.** `src/tui/host_clipboard.rs`: native candidates are wl-copy
   (only if WAYLAND_DISPLAY), xclip then xsel (only if DISPLAY), else nothing; then an OSC 52
   write via the tty with tmux/screen wrapping (`write_osc52_clipboard_to_with_multiplexer`).
   On the user's box only **xsel** exists, the TUI runs inside **byobu/tmux** (OSC 52
   passthrough depends on tmux `set-clipboard`), and reads (SHIFT+CTRL+V) have the same
   fragility. Failures are silent — `publish_system_clipboard` is fire-and-forget with no
   user-visible outcome.
2. **Per-surface coverage gaps.** Browse's own inline editors (`BrowseInlineEditState`,
   app.rs:10223; `EditorTextTarget` contract, app.rs:10236) DO have full mouse selection
   wired — Down begins/double-click selects all, Drag extends, Up ends
   (keybindings.rs ~49363–49420 → `editor_text_input_mut` ~9169 →
   `begin/drag/end_mouse_selection`, text_input.rs ~952) — we verified this during audit,
   so do NOT rebuild it; test it. The uncovered suspects are OTHER text-editing surfaces
   the user may mean by "inline folder/filename editing": the **tui-file-picker overlay's
   own rename/filter/search inputs** (crates/tui-file-picker/src/{input,state,text_input}.rs
   — a separate dispatch from Browse inline edit), tree-pane inline variants, and
   real-terminal event delivery differences under tmux/byobu mouse reporting. Audit every
   text-editing surface for the full copy/cut/paste + mouse-selection contract rather than
   assuming Browse parity.

### Outcomes
**A1 — Copy/cut lands on the host clipboard from every text-editing surface, reliably on
this user's real environment** (tmux/byobu; xsel-only X11; also correct under Wayland and
bare X11). Whatever transport mix you choose (tool spawn order, OSC 52 with correct
tmux/screen wrapping and passthrough handling, retries), it must be robust and testable.
The 2s timeout discipline stays (never block UI).

**A2 — No silent clipboard failures.** When a host mirror/read fails or no transport is
available, the user gets an actionable status line (e.g. "host clipboard unavailable: no
wl-copy/xclip/xsel and OSC52 blocked by tmux — see :clipboard"). Internal copy must still
succeed and say so.

**A3 — A `:clipboard` diagnostic command** (vi command mode, src/tui/command.rs) reporting:
detected environment (WAYLAND_DISPLAY/DISPLAY/TMUX/term), which transports were found,
result of a live write+read self-test, and the last N mirror attempts with outcomes. This
is how the user and we stop guessing.

**A4 — Mouse text-selection works in every text-editing surface** — Browse rename/create/
inline-metadata field, `path:` field, search input, metadata-editor fields, AND the
tui-file-picker overlay's own inputs (rename/filter/search): click to position, drag to
select, highlight visibly rendered, selection survives until an editing key; a mouse
action inside the active editor's bounds must never commit/cancel the editor or fall
through to entry selection / filesystem clipboard semantics. Copy after mouse highlight =
the highlighted text. (Browse's editors already have the wiring — verify/test; bring every
other surface to parity.)

**A5 — Uniform semantics everywhere**: Ctrl+C/Ctrl+X on a selection (or whole field when
selectionless), Ctrl+V/Ctrl+P paste internal, SHIFT+CTRL+V paste host (existing chord —
keep), identical across every text-editing surface in the app. Copy path, Copy tags, and
filesystem cut/copy keep mirroring to host as today (do not regress the existing
host-mirror call sites).

### Guardrails (Part A)
- Byobu-safe input rules: no F-keys; no Shift+Click/Shift+arrows/Ctrl+Space as the only
  path; existing chords keep their meanings.
- The shared text clipboard (`write_shared_text_clipboard` /
  `read_shared_text_clipboard`, tui-file-picker text_input.rs) remains the single internal
  authority; do not add a rival.
- Host I/O stays off the render/event path (workers, timeouts as today).
- The two new reproduction tests must keep passing unmodified; add equivalents for every
  surface in A4/A5 at the dispatch layer (handle_key / mouse handler level, scoped
  clipboard + publish-hook capture — see those tests for the idiom), plus transport-level
  tests with a fake command runner covering: xsel-only, no-tools+OSC52, tmux wrapping,
  write failure surfacing (A2).
- No new heavyweight deps; a small pure-Rust helper crate is acceptable if it buys real
  robustness and builds in the nix sandbox.

---

## Part B — Untaggable carriers (DFF, SHN, DTS, AC3) with sidecar CUEs: never a blank editor

### User-visible failure
`~/torrents/Michael Jackson – Thriller. 1984 Japan` — .dff files + sidecar .cue. ALT+P /
Properties on the folder → **completely blank, uneditable metadata overlay**. Right-click
the .cue itself → Properties / Edit metadata → same blank overlay. The sidecar cue is
ignored despite being a valid metadata source.

### Diagnosis (traced and quoted; verify at will)
1. Lofty cannot read DFF tags. `read_editor_metadata_file` fails and probe.rs:8136-8145
   swallows it: returns `Ok(MergedTagsAndMetadata { entries: Vec::new(), metadata_errors:
   vec![Some(issue)] })`.
2. CUE admission (`admit_split_cue_member`, convert/split_cue_album.rs ~1277) checks
   is-audio, not is-taggable — the surface IS admitted (which is correct per the lodestar).
3. The editor builder (`build_metadata_editor_for_cue_surfaces_with_policy_and_member_
   file_order`, keybindings.rs; single-carrier branch ~19147) constructs a
   `PresentationTab` from the empty entries and returns `Ok` — no `entries.is_empty()`
   check, no surfaced error. The per-file `FileReadState::Unsupported { reason }`
   (keybindings.rs ~21994-22023) exists but only styles technical details.
4. The explicit right-click-the-cue route forces `SidecarOnly` policy and terminates in the
   same construction.
5. DSF is unaffected (custom ID3 path, probe.rs ~8103). **SHN, DTS, AC3 share the DFF
   behavior.** No test covers untaggable-carrier-with-cue anywhere.

### Governing spec
`docs/metadata_source_selection_heuristic.md` (bundled — the LODESTAR; this area regressed
6-7× historically). Structural authority: a selected valid CUE is authoritative for logical
track structure. A carrier's inability to hold tags makes file-tag WRITES infeasible; it
does not invalidate the CUE as a metadata source.

### Outcomes
**B1 — The blank editor is abolished, app-wide.** For ANY combination of carriers and
sidecars, opening Properties either produces an editor with real, honest content, or a
clear status/empty-state ("no editable metadata: <reason per file>") — never a silent
zero-row overlay. This includes untaggable carriers WITHOUT any cue.

**B2 — A valid sidecar CUE over untaggable carriers is a first-class metadata surface.**
The Thriller folder must open as a per-track cue album (synthetic-sheet path — the same
machinery taggable single-image albums use): album + per-track fields visible and
EDITABLE, sourced from the cue. Saves route to the writable representation only — the
sidecar cue (and/or the established metadata-sidecar mechanism; your choice, justified) —
with per-file tag writes marked Blocked/Unsupported exactly as `FileWriteEligibility`
already models. No fake "we wrote tags to DFF" claims anywhere (logs, status, dirty
tracking must reflect the real write targets).

**B3 — Honest per-file state everywhere it matters.** The Details view and any save
summary must show the Unsupported reason for untaggable carriers (the reason string
already exists — surface it). Attempting an operation that cannot apply (e.g. Transfer
tags TO dff) degrades with an explicit message, not silence.

**B4 — Right-clicking the sidecar cue behaves identically to opening the folder** (modulo
the existing explicit-selection policy bypass, which must keep working for taggable
carriers).

**B5 — Sidecar creation for untaggable albums that lack one.** When the user edits
metadata on untaggable carriers with NO existing sidecar cue and saves, tonepoet
materializes a sidecar cue (via the existing CUE-generation machinery — do not write a
second generator) capturing the album/track structure (probed durations/order) plus the
edited fields, and that sidecar becomes the album's metadata surface from then on (same
path as B2). Also provide an explicit user-invoked "create cue sheet" affordance for doing
it without editing first (surface of your choice: context menu and/or vi command).
GUARDRAIL: never create files as a side effect of merely browsing or opening Properties —
materialization requires an explicit save or the explicit action; it must be visible in
the save summary; and it must respect the existing deletion/write safety conventions.

### Guardrails (Part B)
- LODESTAR-governed: do not disturb taggable-carrier selection/priority behavior, the
  single-image guard fixed at ~19092-19130 (regression tests exist), native multi-file
  albums, or genuine 1-track flat cues. Full gate ×2 posture applies to this area.
- `admit_split_cue_member` keeping untaggable carriers admitted is correct — the fix
  belongs downstream (presentation/write-eligibility), not in admission, unless you can
  argue otherwise against the lodestar.
- Cue writes go through the existing cue/sidecar write machinery; do not invent a second
  cue writer.
- Tests: minimum (a) dff+multi-track sidecar cue → editable synthetic album, saves land in
  the cue, file tags untouched; (b) dff without cue → honest empty-state, not blank grid;
  (c) an SHN or DTS variant proving the fix is format-generic; (d) right-click-the-cue
  route for (a); (e) regression: flac+cue behavior byte-identical to today; (f) B5:
  edit+save on a cue-less dff album materializes a correct sidecar and reopening uses it;
  browsing/opening alone creates nothing.

---

## Deliverables
Complete replacement files (or unambiguous per-file patches); architecture summary with
WHY (especially: host-clipboard transport strategy and the untaggable-carrier write-routing
decision); test list; honest statement of anything unverifiable in your environment (you
have no display server, no tmux, no real DFF fixtures unless you synthesize headers).

## Bundle manifest
- This brief.
- `docs/metadata_source_selection_heuristic.md` (LODESTAR, Part B).
- Complete `src/` tree of the main crate (all referenced code: keybindings.rs, app.rs,
  browse.rs, draw_browse.rs, context_menu.rs, event_loop.rs, host_clipboard.rs, command.rs,
  probe.rs, metadata_view_models.rs, message.rs, convert/split_cue_album.rs,
  convert/pipeline/*, dsf_tags.rs, metadata_persistence route, etc.).
- Complete `crates/tui-file-picker/` (TextInputState + selection/clipboard mechanics).
- Root `Cargo.toml`, `CLAUDE.md`.

NOT included (not germane): other workspace crates, `target/`, other docs. If anything you
need is missing, say so explicitly rather than guessing.
