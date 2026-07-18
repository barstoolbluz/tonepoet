# Brief: DSF in-place completion, custom-tag survival, overlay authority, selection contrast, Browse create

HEAD at time of writing: `e958d74` plus this brief's commit; the archive is cut
at the brief-inclusive commit. Verify `docs/handoff_manifest.txt` FIRST
(SHA-256 of every archived file; if your copy is incomplete, say so and scope
down). `docs/handoff_git_history.txt` carries the recent commit log + full
diffs for offline commit review. Standards unchanged (fail closed, structured
signals, value-asserting pins, honest degradation, complete files, I/O cost
statements for changed I/O paths). Every finding below is mechanically
verified at HEAD with file:line evidence; line refs may drift a few lines.

---

# P0

## P0-1. DSF tag writes: the in-place path exists but is unreachable — finish it

Field evidence: after the journaled in-place tail work shipped, editing tags
on a 10-track DSD256 album STILL took minutes. Byte progress works; every
file still fully rewrote.

Root cause (verified, dsf_tags.rs): the in-place test at `write_prepared`
(~613-640) requires `encoded_tag.len() <= allocation` where allocation is the
EXACT byte span of the existing tail (~633) — ripper tags are compact with
zero padding, so any growing edit falls to `rewrite_container`. And the
rewrite is SELF-PERPETUATING: it writes the new tag exact-size with no
padding (~1339, ~1404-1418), so even files tonepoet already rewrote will
full-rewrite again on every future growing edit. Untagged files
(metadata pointer 0) also route to full rewrite (~1320: the "audio prefix"
is the whole file). Batch layer: any DSF in the batch forces ONE worker
(probe.rs `metadata_write_worker_count` ~7186), so 10 files = 10 strictly
sequential ~800 MB rewrite passes ≈ 8 GB of I/O.

Wanted:
- **Seed padding on the unavoidable rewrite** (FLAC prior art:
  `REWRITE_PADDING_BYTES = 1 MiB`, probe.rs ~779/~1825): pad the tag out in
  `rewrite_container` so the NEXT growing edit takes the in-place path. The
  in-place path already preserves surplus allocation
  (`pad_id3_to_allocation` ~642-666), so seeded padding persists. This turns
  every rewrite into the last rewrite for that file.
- **Append-in-place for untagged files**: the DSF tag lives at EOF past all
  audio; first-tagging needs only append(tag+padding) + patch the two 8-byte
  header fields at offsets 12/20 (the same fields `rewrite_container`
  patches at ~1392-1399). Crash-safe ordering is free (append+fsync, then
  header patch — until the pointer is patched the file is a valid untagged
  DSF); the existing tail-journal machinery can cover the header patch.
  Decide and document the journal shape.
- **Then revisit the DSF one-worker rule** (~7186-7196): with in-place costs
  (~2 MB, 3-4 small fsyncs per edit) the serialization is no longer needed
  as a bandwidth shield; keep it only if journal-safety requires it, and say
  why either way.
- Cost statements required (house rule): before/after arithmetic for the
  compact-tag edit, the padded follow-up edit, and the untagged first-tag.
- Pins: growing edit on a zero-padding fixture full-rewrites ONCE then
  edits in place; untagged fixture appends without moving audio bytes;
  crash-recovery pins for the new header-patch journal shape.

## P0-2. Custom tags do not survive conversion (three drop points + an irony)

Field evidence: `add field` → `PRE_EMPHASIS` = `1` in the editor, saved
(verifiably in the source file); converted outputs don't carry it.

Verified mechanism — three independent drop points; which one fires depends
on source shape and target format:
- **Drop A (read)**: `materializer_single.rs::read_track_metadata_with_warnings`
  (~250-297) parses only fixed fields + two hand-picked extras; arbitrary
  keys never enter `TrackMetadata.extra`. Asymmetric with the archive
  materializer (enumerates ALL text items, ~1820-1832) and DSF
  (`to_track_metadata` copies every key).
- **Drop B (write rename)**: `authoritative_metadata_tags` (stages.rs
  ~4105-4218) emits track extras as `TONEPOET_TRACK_<KEY>` (~4209-4215) —
  never the raw key. Album extras have a real-tag allowlist
  (`album_extra_real_tag_key` ~4092); track extras have none.
- **Drop C (write delete/strip)**: `PRE_EMPHASIS`/`CUE_FLAGS` are on
  `AUTHORITATIVE_CUE_MANAGED_TAG_KEYS` (~4334-4356) so raw copies are swept
  by the metaflac/opustags/wvtag delete pass; and for mp3/m4a/aac/wav/aiff
  the metadata stage is an ffmpeg rewrite with `-map_metadata -1`
  (~4526-4552) that strips EVERY tag.
- **The irony**: `TrackMetadata.pre_emphasis: bool` exists and writes
  exactly `PRE_EMPHASIS=1` + `CUE_FLAGS=PRE` when true (~4183-4186) — but
  only the CUE materializer sets it (FLAGS PRE); and tonepoet's own
  pre-emphasis detector treats a user `PRE_EMPHASIS=1` tag as authoritative
  evidence (src/tui/preemphasis/metadata.rs ~66-94) — TUI-side only,
  feeding nothing into the pipeline.
- The LEGACY single-file path preserves arbitrary tags (`-map_metadata 0`);
  only the pipeline path (DSF, albums) drops them.

Wanted: user-authored tags survive conversion end to end.
- Read parity: port the archive materializer's all-text-items enumeration
  into the single materializer.
- Write policy: decide raw-key passthrough for user-originated extras
  (a track-level allowlist, provenance marking, or pass-through-unless-
  internal — your call, documented); reconcile the managed-key deletion so
  a user's PRE_EMPHASIS is not swept (e.g. delete only when the pipeline
  owns a pre_emphasis fact); replace `-map_metadata -1` for the ffmpeg
  rewrite formats with `-map_metadata 0` + targeted deletes or explicit
  `-metadata` emission of preserved extras.
- Wire the flag: set `TrackMetadata.pre_emphasis` from source-tag evidence
  at materialize time (reuse `is_affirmative_preemphasis_value`), giving
  the tag first-class survival through the existing writer.
- Re-run convergence must hold (the delete sweep only removes TONEPOET_*
  and managed keys today — keep user keys re-run-safe).
- Pins: PRE_EMPHASIS=1 survives single-file AND album conversion to FLAC,
  Opus, WavPack, MP3, and AAC; an arbitrary custom key (e.g. MY_NOTE)
  survives the same matrix; re-run converges.

## P0-3. Overlay authority for the AR/CTDB/Analysis completion family (+ :password)

Verified (event_loop.rs): `AnalysisComplete` installs `ActiveOverlay::Analysis`
unconditionally (~3250/3256) — `:analyze` from the editor parks the session,
the editor is restored, the user keeps editing, then the async completion
REPLACES AND DROPS the dirty editor. Same class: `CtdbComplete` (~4444),
`AccurateRipComplete` (~4893/4907/4927), `ArBatchComplete` (~4475), and
`OffsetCorrectionComplete`/`CtdbRepairComplete` which set the overlay to
None (~4481/4493), dropping whatever is open. MB/GNUDB/CUE completions all
carry operation authority (the pattern you built twice); this family has
none. Also: `Command::Password` (command.rs ~3474) installs TextEdit with no
parked-editor check — a session parked by the editor's `:` route strands
invisibly and can later be dropped by slot-nulling paths.

Wanted: give the AR/CTDB/Analysis family the same operation-scoped authority
treatment as MB/GNUDB (ids on the messages, checked at the handlers; parked
sessions restored on failure/retirement; no unconditional overlay installs;
overlay-to-None arms must not drop a live editor), and make `:password`
parking-aware. The prior art is in this same file — match its shape. Pins
per completion: a completion arriving over a dirty editor/parked session
must not destroy it.

---

# P1

## P1-1. Inline-edit selection contrast is a DESIGN bug in every theme

Field evidence (Tokyo Night): highlighted text during inline editing is
nearly indistinguishable — "the user has to squint... even then they aren't
sure."

Verified: it is structural, not palette tuning. The only selection painter
(`render_inline_value_with_embedded_cursor`, inline_edit.rs ~75-144) uses
`fg = theme.bg` on `bg = selection_bg` — and `input_focused_bg` is DERIVED
as a 3:4 mix of panel_bg and selection_bg (theme.rs ~160), so the selection
region differs from its surroundings by a 25% admixture BY CONSTRUCTION.
Measured across all 24 built-in themes: selected-fg-vs-selection-bg 1.14-2.04
(needs 4.5), selection-bg-vs-field-bg 1.03-1.21 (needs 3.0). No theme
passes. There is no `selection_fg` role anywhere. A fg swap to `text_bright`
alone yields 4.2-14.1 (only solarized-dark 4.16 and tokyo-night-day 4.37
fall slightly short) — one existing site already does this correctly
(conversion_actions_ui.rs `selected_style`).

Wanted:
- Fix the pair structurally: introduce a proper selection text style (e.g. a
  hand-set `selection_fg` role, or an inverse-video pair, or a dedicated
  text-selection bg distinct from the row-selection bg) such that BOTH
  numbers clear WCAG-ish thresholds in all 24 themes; adjust the two
  slightly-short palettes if you take the fg-swap route. The styling is
  centralized in inline_edit.rs (all metadata/browse/gnudb edit sites route
  through it — enumerated in the research; verify), so one fix inherits.
- Theme system: `selection_bg` is a hand-set builder role; wire any new role
  through the palette structs, builder slots, quantization, and theme-file
  serialization (theme.rs ~39/~522/~894/~1095), with serde default for
  existing custom theme files.
- ALSO: the Convert screen's inline inputs (`render_inline_value`,
  inline_edit.rs ~37-64; used by draw_metadata.rs/draw_output_options.rs)
  render NO selection at all despite opening with select-all — the user
  cannot see that typing will replace the value. Route them through the
  selection-aware renderer. Check the theme builder's own text inputs (they
  bypass inline_edit's renderers).
- Pins: a compute-contrast test over all built-in palettes asserting the
  two ratios clear thresholds (this prevents the next palette regressing).

## P1-2. Browse: "New" on empty-space right-click + `:new-file`/`:new-folder`

Feature. Verified mechanics: right-click has NO empty-space branch — it
opens the ENTRY menu for whatever row was selected (`open_context_menu`,
keybindings.rs ~6061-6086; `build_browse_empty_menu` fires only in an empty
directory and has no create actions). Create code already exists in the
tui-file-picker crate (`try_create_named_item`: `create_new` no-overwrite
files, `fs::create_dir` folders, unique-name defaults) — reuse its
semantics. The flow template is rename-in-row (`BrowseInlineEditState` +
`commit_browse_rename`, keybindings.rs ~27370-27461: empty/separator/
exists/archive-mode guards, then op → `refresh_with_search` → cursor
reposition → status).

Wanted: empty-space right-click (click inside the file-list pane with no
entry under the pointer) opens the empty menu even when a selection exists;
add New file / New folder entries there (and optionally to the entry menu);
naming prompt via a new `BrowseInlineEditTarget::Create { dir, kind }`
seeded select-all with a unique default name; create with
`OpenOptions::create_new` / `fs::create_dir` (single validated component, no
`create_dir_all`), guards copied from the rename exemplar incl. `.`/`..`
rejection and archive-mode refusal; `:new-file [name]` / `:new-folder
[name]` (with arg: create directly; without: open the prompt), registered in
parse_command + the completion candidates. Pins: create/refresh/cursor,
no-overwrite refusal, archive-mode refusal, command parsing.

## P1-3. Conversion log: gate the DSD auto-gain margin line

Verified: `append_dsd_settings` (stages.rs ~14642-14662) prints "DSD auto
gain margin" unconditionally for DSD→PCM, even under "DSD gain mode:
disabled". Gate the margin line on gain mode == Auto; symmetrically gate the
"DSD manual gain" line on mode == Manual (today it gates on value presence
only). Update/extend the pin at ~36725 (Auto keeps the line; add a
Disabled-mode assertion that it is absent).

## P1-4. DSF artwork writes: bring them onto the journaled path

Verified (prior audit): artwork writes still take the legacy full-file
path — `write_artwork_one_file` → `write_artwork_lofty_with_backup`
(probe.rs ~7785/~7863): full-file backup copy + full-file lofty rewrite, no
journal/cancel/byte-progress; a crash mid-artwork mints a `.tonepoet-bak`
marker that then BLOCKS ALL TAG SAVES on that file (preflight refusal,
dsf_tags.rs ~1537) until manual dsf-recover. Route DSF artwork through the
dsf_tags container writer (artwork frames are ID3 APIC — the same tail),
with the same journal/cancel/progress/cost properties as P0-1. Non-DSF
artwork stays on its current path.

## P1-5. ReplayGain provenance completeness + log honesty

Verified residuals from the prior audit: `inherited_replaygain_tag_policy`
(stages.rs ~7201-7255) detects lossy targets, DSD gain, and rate change —
but NOT bit-depth reduction or float→int conversion (24→16 with dither
skips the scan and trusts a 24-bit PEAK), and `RateTarget::Source` with an
unknown-rate track is non-conservative (`_ => false` at ~7226 vs the
explicit-rate branch's `_ => true`). The conversion log carries NO
inherited-vs-recomputed provenance (trust/recompute reasons go only to
log::info; "Skipped" is indistinguishable between disabled/zero-tracks/
trusted). Also `prevent_clipping` is dead: loudgain `-k` is hardcoded
(~7363/7374) regardless of the setting. Wanted: add depth-reduction and
float→int to the recompute predicate; make the unknown-rate Source branch
conservative; record RG provenance in the log (which policy fired, why);
honor or remove `prevent_clipping` (your call, documented). Verify loudgain
actually strips stale ALBUM_* in track-only mode or compensate (~7360).

## P1-6. Tolerant-read warnings must reach the durable conversion log

Verified: DSF container quirks reach the reporter (transient) and the
editor UI, but `PreparedSource`/`PreparedTrack` carry no warnings, so
`build_conversion_log` persists nothing — a conversion that proceeded on
degraded metadata leaves no durable trace. Carry materializer warnings into
the prepared structures and render them in the log's per-track details.

## P1-7. Secrets hardening residuals

From the prior audit (all verified, file:line in
docs/handoff_git_history.txt context):
- Pending publication journal + corrupt store still BRICKS startup
  (config.rs ~1502-1516 degrades only on Unavailable; parse/Corrupt errors
  propagate through require_startup_config) — every subcommand incl.
  `config --reset`. Degrade on ALL journal-reconcile errors at LOAD (keep
  strict on save), and name the remedy files in the error.
- Headless wedge: a leftover journal makes every config save fail on a
  dbus-less box (strict reconciler falls through to native keyring for
  revocation). Give the save path a headless-safe retirement or an explicit
  `config --retire-secret-journal` escape hatch.
- `clear_queue` (mod.rs ~1292) is the ONE queue mutation that doesn't
  retire queue-owned secret references — permanent orphans. Align with
  clear_all/clear_completed/clear_finished.
- One unresolvable MRU reference bricks the whole password list
  (keychain.rs ~41-53 hard-errors on first failure) — degrade per-entry
  with a visible skip warning.
- Browse-add dedup asymmetry: `add_file_ready_for_processing` has no
  dedup/replacement at all while Convert commit has both — document as
  intended or align.

---

# P2 (fix if the round has room; else report)

- Startup ordering strands DB PREPARED entries: the directory scan retires
  byte-identical markers BEFORE `recover_stale_metadata_writes` runs
  (event_loop.rs ~39/42) — swap the order or make DB recovery
  marker-absent-tolerant for the byte-identical case.
- `dsf-recover` cannot touch tail journals (main.rs ~375-413 wraps legacy
  markers only) — extend it; error messages point users at a tool that
  can't help. Also `restore-backup` should header-sniff the marker
  (`DSD `) before copying it over a healthy file.
- Non-UTF-8 DSF filenames collapse to one shared journal name
  (`tail_journal_path` ~669 falls back to "audio.dsf") — can dead-bolt an
  innocent same-name file; derive a collision-free name (hash the OsStr).
- GnudbReview disc-pill click clears the in-flight edit rather than
  committing (keybindings.rs ~19123) — align with the row-click
  commit-to-old-row behavior.
- `set_source_mode` and `set_source_mode_preserving_format_selection` are
  byte-identical with docs describing a difference that doesn't exist
  (app.rs ~4997/~5011) — merge or differentiate.
- The sentinel-clamp status can fully overwrite a probe warning
  (event_loop.rs batch reducer) — combine instead.
- Test hermeticity: two mouse pins depend on the 80x24 terminal-size
  fallback (keybindings.rs ~18596) — inject the size or compute
  coordinates from the code's own layout call.
- Multi-value editor rows remain lossy on edit (join with "; ") — at
  minimum document in the UI; better, preserve unedited values (already
  true) and warn when an edit collapses multiple values.

# Explicitly out of scope

- Companion-cue product decision: STILL OPEN with the user; do not change
  companion behavior.
- DFF tag writing; ambiguous-EAW terminal handling.

# Delivery contract

Unchanged: complete files, one archive, IMPLEMENTATION_REPORT with files/
assumptions/cut-list/I-O-cost statements, value-asserting pins, weakened
tests justified. New dependencies: declare in Cargo.toml against documented
APIs, NEEDS-VERIFICATION seam for anything unverifiable offline; no
hand-rolled crypto. Acceptance on our side: `cargo test --workspace` green
(4371 at HEAD), zero cold-build warnings, `TONEPOET_REQUIRE_TOOLS=1 cargo
test --test depth_format_matrix` green. Priorities are ordered: P0 fully
correct beats P2 breadth; if an item can't be done to standard, ship the
subset that can and say what you cut.
