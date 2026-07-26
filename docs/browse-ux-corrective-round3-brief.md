# Corrective Brief — Browse UX Round 3

Field feedback on the round-2 delivery (applied at d4b0d85, tree @ 5620d58). Six items.
User environment matters this round: the user drives tonepoet over SSH from **Windows
Terminal**, which binds Ctrl+V to its own paste action — the app NEVER receives a
Ctrl+V KeyEvent from that terminal; it receives a bracketed-paste `Event::Paste`
carrying the Windows clipboard instead. Field-tested: **Ghostty behaves the same way**
(also intercepts Ctrl+V). **xfce4-terminal delivers Ctrl+V as a key**, and there BOTH
paste planes work correctly today — in-editor text paste and filesystem item paste —
which validates the round-2 architecture end-to-end and isolates the defect class to
terminals that intercept the chord.
Line anchors from 5620d58 — re-locate before editing.

## 1. Clipboard chords must survive terminals that steal Ctrl+V

**Field reports:** (a) Ctrl+X on selected text in the Browse inline editor, then Ctrl+V
in the same editor → the WINDOWS clipboard is pasted, not the cut text. (b) Ctrl+X on a
folder, Ctrl+V in another directory → status: "terminal paste has no focused text
editor; Ctrl+V is reserved for file paste" and nothing pastes.

**Diagnosis (verified):** the round-2 architecture is internally correct and both
symptoms are the same root cause — the Ctrl+V KeyEvent never arrives:
- In-editor text cut/paste IS fully wired: the shared editor handles Ctrl+X →
  `cut_selection` and Ctrl+V → `paste_clipboard` against the in-app clipboard
  (`crates/tui-file-picker/src/text_input.rs:783-788`, static clipboard at :7-10), and
  the browse inline-edit dispatcher runs it (`keybindings.rs:~2135-2162`, with the
  "text clipboard is empty; terminal clipboard paste arrives via bracketed paste"
  status when paste finds nothing).
- File paste IS wired: Ctrl+V key in the list → `PasteSelection`/`TreePaste`
  (`keybindings.rs:~3909`).
- `Event::Paste` routing (round 2) inserts terminal text into whichever editor is
  focused (`event_loop.rs:5655-5830`) and errors at :5829 when none is.
- On Windows Terminal every one of those Ctrl+V paths is DEAD — the terminal converts
  the chord to `Event::Paste`. Report (a) is the paste event winning inside the
  editor; report (b) is the no-editor error branch, which on that terminal is
  unreachable-by-design rather than a user mistake.

**Required:** both clipboard planes (in-app text, filesystem) must be fully usable on a
terminal that never delivers Ctrl+V. Design is yours, but it must include:
- A file-paste trigger that works there. Natural candidate: when `Event::Paste`
  arrives with NO focused text editor and the filesystem clipboard is non-empty,
  treat it as the file-paste command (discard the text payload; the current error
  branch at event_loop.rs:5829 becomes this). Decide and document the
  empty-filesystem-clipboard case (keep a clarified message).
- The alternate chord is DECIDED by the user: **Ctrl+P** = paste, context-sensitive
  exactly like Ctrl+V (focused text editor → in-app text paste; browse list/tree →
  filesystem paste). Ctrl+P is verified unbound today. Ctrl+B is NOT available (it is
  the bookmarks dropdown). If you need companion chords (e.g., a cut/copy analog for
  symmetry), propose them in the report — but Ctrl+P-for-paste is fixed. NO function
  keys; NO emoji in labels; Ctrl+Q stays quit. Document the chords in the help/footer
  surfaces where the old ones appear.
- Keep the existing Ctrl+V key behavior for terminals that do deliver it (Ghostty
  et al.) — additive, not a rebind.
- Regression tests: Event::Paste with no editor + non-empty filesystem clipboard
  starts a paste; with empty clipboard produces the guidance message; with editor
  focused still inserts text; alternate chords covered.

## 2. Text selection invisible inside the inline editor (root cause found)

**Field report:** selected text in the inline editor is indistinguishable from the row
highlight bar; only the cursor is visible.

**Diagnosis (verified):** the editor's selection styling is correct inverse-video
(`inline_edit.rs:42-46`: fg=bg, bg=text_bright). The bug is downstream: the
selected-row restyle pass in `draw_browse.rs:2573-2585` OVERWRITES every span of the
selected row — `span.style = span.style.bg(selection_bg)` plus a forced
`fg(text_bright)` — including the inline editor's spans. The editor's selection (and
its field background `input_focused_bg`) are clobbered into exactly the row-bar
colors. The cursor survives only because its glyph differs.

**Required:** the row-highlight restyle must not repaint the inline editor's cell
(exempt those spans, or order the passes so the editor renders after/over the bar).
The editor must visibly show: field background distinct from the row bar, selection in
inverse video, cursor as today. Verify the same fix covers the path-bar editor if it
can ever coincide with a restyled row. Secondary gap (fix or explicitly defer with a
note): the search input and filter input never render text selection at all
(`draw_browse.rs:~2191-2199, ~2353-2360`) while path bar and inline rename do.
Regression: a render test asserting the selection span's style differs from the
row-bar style on a selected row.

## 3. Native-rename proof falsely reports failure on cifs (move actually succeeded)

**Field report:** cut/move on cifs → "Failed: <item> — move committed at <path>, but
the native rename transition could not be proven: source changed while native rename
committed: …" (truncated). BUT the move succeeded: source gone, destination complete.

**Diagnosis (audit-corrected):** cifs IS classified reduced → `ContentVerifiedPortable`
(`source_guard.rs:252-310` type table). The quoted message is the STRICT-branch wording
from the retained-handle re-proof (`verify_committed_rename`, `source_guard.rs:862-944`;
strict compare at :909-912 wraps "source changed while native rename committed").
Runtime observation upgrades are NOT the route — `merge_observed_identity`
(:334-347) unconditionally returns `Unsupported` on NetworkOrReduced semantics; it can
only pin down, never promote. **The identified defect: line 909 selects the comparator
for the retained SOURCE handle using `destination_capabilities.identity_policy()`.**
The handle's stat semantics belong to the mount that owns the file; attributing them
to the destination's classification is wrong by construction. How the strict policy
concretely arose for the user's operation must be determined empirically — candidates:
(a) the `FilesystemSemantics::Unknown => observed` arm of `merge_observed_identity`
(:346) IS a real upgrade path — a mount whose statfs magic is not in the
classification table gets Unknown semantics, and a favorable runtime identity probe
then yields Supported, which can produce a Strict-grade policy on a mount that
deserves portable treatment (plausible for exotic cifs/smb configurations); (b) the
destination path's capabilities being probed pre-creation or resolving to a different
mount than the file (mount-boundary/parent-walk attribution); (c) capability
cache-key behavior across mounts; (d) an ntfs-vs-cifs difference between the user's
two test environments. Instrument if needed; do not guess.

**Required:**
- Handle-based comparisons must use the capabilities of the filesystem that owns the
  handle/file being compared — never a differently-classified peer path's policy. On
  NetworkOrReduced/Unknown semantics the committed-rename proof must accept the
  documented portable evidence (retained handle, type, size, path transition —
  exactly what the round-2 report §4.1 promised).
- A rename that verifiably committed (source absent, destination present, portable
  evidence consistent) must never be reported as `Failed`. Worst case on proof doubt:
  completed-with-warning, with retry state NOT seeded (downstream today:
  `record_committed_failure` → root Failed, excluded from completed stats, retry
  seeded — `keybindings.rs:~27800-27840`).
- Regression tests simulating the cifs behaviors (post-rename handle snapshot with
  changed inode/timestamps under a reduced-capability injection) proving disposition
  and message.

## 4. Long-diagnostics surface — round-2 requirement, dropped; re-stated

The round-2 brief required an inspectable surface for long failure/retention text.
Partially exists: the live `FileTaskProgress` overlay does receive and display error
records DURING a task (`RecordError` routing, `keybindings.rs:~27391`,
`event_loop.rs:~1022`). The gap is AFTER completion — once the overlay closes, the
only surface is the status bar, which silently ellipsis-truncates at width
(`draw_footer.rs:145-167`); full messages survive internally
(`committed_root_failures`, `root_results[].message`, `terminal_error`) with no
post-completion view. Both cifs incidents this round truncated mid-sentence in
exactly that state. **Required this round, not optional:** post-completion review of
the full failure/warning text of the most recent file task (reopenable summary,
`:messages`-style log, wrapped detail overlay — your choice), reachable by keyboard
and mouse, no F-keys; plus line-wrapping wherever these messages render.

## 5. Degrade-warning verbosity: quiet by default (end-state directive)

Renames/moves on cifs/ntfs now work-with-warnings; the user wants routine degrade
notices ("native rename was accepted using retained-handle… evidence",
`keybindings.rs:~27807-27809`; "filesystem lacks atomic no-clobber rename; used a
checked best-effort rename", `:~30059`, crate `state.rs:~3783`; the identity-policy
notice, crate `source_guard.rs:488`) suppressed in normal use once the machinery is
trusted. Implement now as a switch: default QUIET for per-operation degrade notices on
capability-classified mounts (first occurrence per mount per session may inform once),
full verbosity under a debug/verbose toggle (config key and/or `:command`). Failures
and data-affecting warnings are never suppressed. Document the toggle.

## 6. Double-click must not enqueue non-audio files

**Field report:** double-clicking `cover.jpg` switches to Convert, source pane shows
the album/track list, status shows `Probe failed: no audio stream found in "<path>";
set format manually`. Long-standing (predates this branch; fallthrough introduced
2026-07-04 at cc9efde4).

**Diagnosis (verified):** `activate_browse_entry` (`keybindings.rs:1483-1528`) handles
ParentDir/Directory/Archive/in-archive-audio, then UNCONDITIONALLY falls through
(:1526-1527) to `load_browse_selection_pub` → `install_convert_source_with_async_probe`
(:4739-4816) which installs the source, switches screens, and spawns the probe — for
ANY file kind. The centralized admission policy that already rejects non-audio
(`src/convert/source_admission.rs:1-31`, `is_direct_queue_source_path`: audio ✓, cue ✓,
supported archives/disc images ✓, everything else ✗) is NEVER consulted on this path —
it guards only programmatic queue additions. Enter, by contrast, just toggles
selection (`keybindings.rs:~4188-4191`).

**Required:**
- Gate double-click activation through `is_direct_queue_source_path` (the existing
  policy IS the decision — do not fork a second list). `.cue` stays activatable by
  design.
- Define what rejected kinds do on double-click instead: viewable text files → the
  existing View flow (`is_viewable_text_file`, ViewFile); everything else (images,
  unknown) → status message, no screen switch. No new glyphs.
- Regression tests: double-click on jpg/txt/unknown does not change screen or install
  a source; audio/cue/iso behavior unchanged; Enter behavior unchanged.
- Apply the same admission gate to any other activation entrances that bypass it
  (audit `load_browse_selection` callers).

## 7. Constraints (standing)

- NO function-key bindings; NO emojis/decorative unicode; Ctrl+Q stays quit.
- Preserve all green round-2 behaviors and tests (5026/0 baseline); this round adjusts
  and completes, it does not reshape the proof architecture.
- Two-pass rendering; crate builds standalone; `cargo test --workspace` zero failures,
  untruncated.
- Deliverable: overlay tar.gz + MANIFEST + ENGINEERING_REPORT (per-item resolution
  incl. the empirically confirmed item-3 mechanism and your chord choices for item 1)
  + SHA256SUMS. Request missing files rather than guessing; record preimage hashes
  from THIS bundle if you patch blind.
