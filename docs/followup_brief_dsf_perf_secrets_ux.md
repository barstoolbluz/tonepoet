# Follow-up brief: DSF write performance, secrets policy, MB/queue routing, editor & browse UX

HEAD at time of writing: `4d3107a` plus this brief's commit. This round follows
the applied v7 delivery: the apply succeeded (suite 4261/0, zero warnings), a
three-way adversarial audit of the result produced the findings below, and four
empirical field reports from real DSF use are folded in. Every claim below was
mechanically verified at HEAD with file:line evidence; line refs may drift a
few lines by the time you read them — re-verify against the shipped tree.

## Source integrity — read first

Last round you received a truncated 35-file subset and correctly refused to
claim the commit-review requirement was met. This archive fixes both causes:

- `docs/handoff_manifest.txt` lists EVERY file in the archive with its SHA-256
  (generated at archive-cut time — it exists only inside the archive, not in
  the repo history). Verify the archive against it FIRST; if anything is
  missing, say so in your report and do not guess at absent code.
- `docs/handoff_git_history.txt` contains `git log --stat` for the last 40
  commits and full diffs of the four local-fix commits (`7eb466e`, `6a56090`,
  `bdb0a43`, `afffe61`) plus the apply commit `4d3107a` — the commit-level
  review the last brief asked for is now possible offline.

Standards are unchanged from the previous brief (fail closed, structured
signals, value-asserting pins, honest degradation, complete files only). One
addition: **when you change an I/O path, state its worst-case cost** (bytes
moved, fsyncs) in the implementation report — this round exists partly because
a correct-but-quadratic write path shipped without that arithmetic.

---

# P0 — usability blockers (all confirmed in real use)

## P0-1. DSF tag saves are quadratically expensive, uncancellable, and mute

Field evidence: saving edited tags on a 10-track DSD256 album (~400 MB/track)
showed a static "Saving..." for 2+ minutes; Esc took ~3 further minutes to
take effect; 8/10 tracks were written at cancel.

Mechanism (verified):
- Every DSF tag edit rewrites the ENTIRE file TWICE: a full-file rollback
  backup copy + fsync (`dsf_tags.rs::apply_with_backup` ~386 →
  `db.rs::create_backup` ~2887), then a full-file temp rewrite of the audio
  prefix + fsync + parent sync (`dsf_tags.rs::rewrite_container` ~579-664).
  There is NO in-place tag update even when the new ID3 fits the existing
  tail region. ~4× file size of I/O and 3 fsyncs per track.
- Up to 4 tracks are rewritten in parallel
  (`probe.rs::metadata_write_parallelism` ~7025) — concurrent multi-hundred-MB
  copies on one device seek-thrash each other.
- Cancellation is checked ONLY before each file starts (`probe.rs` ~7057);
  `write_with_backup` takes no cancel parameter; `std::thread::scope` joins
  all in-flight writes. The FLAC path has mid-stream cancel checks
  (`copy_stream_bounded`, probe.rs ~2658) — the DSF path has none.
- "Saving..." is a static string (`draw_overlays.rs` ~4627); progress events
  fire only per COMPLETED file (`keybindings.rs` ~6710-6727).

Wanted (design is yours; these are the constraints):
- In-place tail rewrite when the new tag fits between `metadata_offset` and
  EOF (the DSF tag-at-tail invariant makes this a bounded write with no
  prefix copy); full rewrite only when it grows past the allocation. Decide
  and document the durability story for the in-place path (the current
  temp+rename atomicity is a real property — don't silently lose it; a
  bounded tail journal or the existing backup marker may serve).
- The redundant standalone backup copy should be reconsidered where
  temp+rename already preserves the original inode until publish.
- Cancellation checked inside every bounded copy loop (FLAC precedent),
  per-file AND byte-level progress events feeding the Saving footer, and a
  parallelism policy that acknowledges same-device full-file rewrites
  (1-2 workers, or size-aware).
- Related crash-safety gaps (same subsystem, fix together):
  - Orphaned `.tonepoet-bak` markers permanently block future DSF saves
    ("rollback marker already exists", db.rs ~2897) and NO startup scavenger
    consumes standalone markers (directory recovery handles only the FLAC
    journals, probe.rs ~1154-1220). A crash mid-album leaves N gigabyte-scale
    markers + N blocked files.
  - Orphaned `.{name}.tonepoet-id3-{uuid}.tmp` temps are cleaned only on
    in-process error (dsf_tags.rs ~660); nothing scavenges them after a crash.
  - Marker removal on success skips the parent-dir sync (dsf_tags.rs ~395)
    unlike `db.rs::remove_backup_marker` (~2791) — a durably committed write
    can coexist with a resurrected marker after a crash.

## P0-2. Secrets: switch the Linux default to an encrypted file store; fix the startup brick

USER POLICY DECISION: on Linux, the OS keyring (dbus/secret-service) must NOT
be the default backend — default to an encrypted local file store. On macOS
the native keychain works well and stays the default. These are archive
passwords, not crown jewels: optimize for zero interactive ceremony and
headless robustness over maximal secrecy. Keep the `secret_store` seam — this
is a backend swap plus policy fixes, not a redesign.

Confirmed defects to fix regardless of backend:
- **Startup brick (HIGH)**: config load eagerly rehydrates
  `archive_password_ref` via `secret_store::get` and ABORTS load on error
  (config.rs ~1618-1622; journal reconcile can also abort, ~1516);
  `main.rs::require_startup_config` (~361) runs before dispatch, so on a
  headless/locked-keyring box EVERY subcommand fails — including
  `config --reset`, the recovery tool. Rehydration must be lazy (resolve when
  a password is actually needed) and load must never fail on secret-backend
  unavailability.
- **History destruction (MEDIUM)**: queue load runs
  `restore_archive_password_after_load` on EVERY item regardless of terminal
  status, and `fail_closed_for_unavailable_archive_password` flips items —
  including Completed history — to Failed and persists that
  (queue.rs ~494-501, ~531-551; sanitize-rewrite mod.rs ~2404). Terminal
  items must never be rewritten by secret availability; only genuinely
  pending items need their secret, and only at execution time.
- **Per-frame retry (MEDIUM)**: the settings screen calls
  `keychain.ensure_loaded()` every rendered frame (draw.rs ~122); on a
  failing backend that re-drives the file lock and dbus each frame
  (potentially re-spawning unlock prompts). Retry belongs on explicit user
  action, not in the draw loop.
- **Lock contention (MEDIUM)**: store locks are `try_lock_exclusive` against
  a pre-existing sidecar (config.rs ~466, keychain.rs ~36) — a second
  tonepoet process fails startup instead of briefly blocking. Bounded
  blocking (or retry with small backoff) is the wanted behavior.
- Transient-vs-permanent failure classification (secret_store.rs ~519-535
  flattens everything to strings); no secret GC (queue-item refs minted at
  queue.rs ~452 are never revoked; keyring accumulates orphans);
  `TONEPOET_ALLOW_INSECURE_TEST_SECRET_STORE` is honored in release builds
  (secret_store.rs ~15, ~482) — must be cfg(test)/cfg(debug_assertions) or
  removed; "N re-converting" success message counts skipped Completed dupes
  (mod.rs ~715-720, command.rs ~7389).

Encrypted-file backend latitude: pick the scheme (a small
authenticated-encryption file under the config dir with a machine-local key
is acceptable given the stated threat model; an interactive passphrase is
NOT acceptable — no prompts on startup or on ordinary conversions).
Dependency policy: you may add new crates by declaring them in Cargo.toml
and writing against their documented APIs — same as last round's `id3` and
`keyring` (neither was vendored; both worked). Mark any API you cannot
verify offline as NEEDS-VERIFICATION behind a thin seam and the applying
side fixes signatures at apply time. A pure-Rust AEAD crate
(e.g. `chacha20poly1305`) is a reasonable choice; do NOT hand-roll
cryptographic primitives. Migrate
existing keyring references and any surviving cleartext transparently; keep
migrations one-shot, locked, and backed up like the current ones. The
existing crash-recoverable publication journal semantics must survive the
backend swap.

## P0-3. `:tags-mb` cannot apply to plain-file albums (pre-existing, v40-era)

Field evidence: MB search on a DSF folder finds candidates; selecting one
yields `:tags-mb: could not open split CUE editor: No CUE/image pairs
selected`.

Mechanism (verified): the Browse-origin MB apply completion routes through
`build_metadata_editor_for_cue_surfaces_with_mb_release`
(keybindings.rs ~10989-11024); a folder of plain audio admits zero cue
surfaces, control reaches `build_metadata_editor_for_cue_surfaces(app, &[], 0)`
which returns `Err("No CUE/image pairs selected")` (~10722), surfaced at
event_loop.rs ~6818. The empty-surfaces case should return `Ok(None)`: the
existing `Ok(None)` handler (event_loop.rs ~6806-6815) already opens the
plain multi-file editor and `populate_editor_from_mb_scoped` (~6869) already
populates it. Honor the alien-only degrade policy the ordinary editor open
uses (keybindings.rs ~15180-15197): a folder whose only cue is alien should
also degrade here; a genuine local cue album must keep cue-shaped routing.
Affects every non-cue album, not just DSF. Pin both directions.

## P0-4. Queue: Failed items block re-enqueue from the Convert screen (v40-era)

Field evidence: "enqueue + start" on an album whose items previously FAILED
reports "commit: all 10 file(s) already queued"; only clearing the queue
unblocks.

Mechanism (verified): `ConversionManager::commit_batch_with_cue_artifacts`
(mod.rs ~706-714) skips any batch path with ANY existing queue item matching
`same_path_for_queue` — no status filter. The previous status-aware filter
`is_active_commit_item` (convert_actions.rs ~574-589: Failed/Completed/
Cancelled are re-addable) became DEAD CODE when d28f081 rerouted commit.
Restore status-aware semantics on the commit transaction (terminal-status
items must not block; decide whether they are replaced or duplicated —
replacement of the terminal item is the less surprising choice), fix the
"N re-converting" message to tell the truth, and remove or rewire the dead
`commit_batch`/`is_active_commit_item` pair so this cannot fork again.
Browse-screen direct adds have NO dedup at all (keybindings.rs ~4185 →
add_file_ready_for_processing) — decide and document whether that asymmetry
is intended.

---

# P1 — correctness and promised UX

## P1-1. Source pills vs cascades: deliberate "source" selections get clobbered

- `cascade_pcm_source_defaults` (app.rs ~3661) and
  `cascade_dsd_source_to_pcm_defaults` (~3690) unconditionally `select_value`
  over the current selection when a new source is probed
  (`apply_source_info_defaults` ~4523) or on DSD→PCM format transitions. A
  staged rate/depth of "source" — chosen precisely to be source-relative —
  is silently replaced with the cursor file's numeric rate; a mixed-rate
  batch then resamples members. Sentinel/Source selections must survive
  cascades (the v7 pin covers clamp/fallback only — extend it to cascades).
- DSF/DFF target + rate="source" with a PCM source stages a plan that can
  only fail at plan time ("PCM to DSD requires an explicit DSD target rate",
  plan.rs ~1596-1604) while the size estimator treats the PCM rate as a
  1-bit rate (~64x under, draw_output_options.rs ~762-769). Either gate the
  sentinel for DSD targets on `source_is_dsd`, or surface the invalidity in
  the TUI before queueing; fix the estimate either way.
- `:set rate` / `:set depth` (command.rs ~12505-12565) bypass
  `after_user_selection` entirely (no auto-dither/resampler cascade, no
  constraint reapply) — unlike `:set format` and the key/mouse paths. Align
  them. Nit: `:set rate source` status prints "rate = source kHz" (~12523).

## P1-2. ReplayGain SkipIfComplete trusts tags copied from the source

`apply_metadata` (stages.rs ~22692) copies source tags — including
`REPLAYGAIN_*` — onto the fresh artifacts BEFORE `apply_replaygain`
(~22745); the completeness probe (~7107-7127) then sees source-inherited
gain/peak values measured against the ORIGINAL encoding and skips the scan.
After a level-altering conversion (DSD-to-PCM gain, resampling, lossy
encode) those values are wrong. Wanted: the skip policy must only honor RG
tags that are valid for the OUTPUT audio — strip or ignore inherited RG tags
whenever the pipeline changed the signal (you know exactly when: gain
applied, rate changed, lossy encode), and log which policy fired. The
skip-if-present feature itself is correctly wired otherwise (verified:
pill → settings → stage, per-mode gain+peak completeness, all-artifacts
semantics, rescan on unreadable).

## P1-3. DSF reads: strict writes, tolerant reads

`inspect_dsf_metadata_location` refuses `declared_size != actual_size`
(dsf_tags.rs ~133-138) and any chunk layout that doesn't tile exactly
(~242-333). Correct for WRITES. But `read_track_metadata`
(materializer_single.rs ~202-207) now fails closed on the same validation,
so a DSF with a benign header quirk (padded/misdeclared sizes from real
rippers) cannot be CONVERTED at all — pre-bundle it converted with default
metadata. Reads should degrade to best-effort (or empty) metadata with a
visible warning; writes keep the strict gate. Same for the editor read path
(a quirky file should open read-only or with a warning, not vanish).

## P1-4. DSD rate display merge

Wanted rendering in the metadata editor Details tab:
`Sample rate:  11289600 Hz (DSD256)`. Both halves exist: the Details tab
prints `"{} Hz"` (metadata_view_models.rs ~128-133 disc, ~184-187 file);
`tonepoet_pipeline::DsdRate::from_hz` is public and gives the name. Append
` (DSDxxx)` when `from_hz` resolves. The source pane's existing
"DSD256 (11.3 MHz)" rendering stays as is.

## P1-5. Metadata editor: Tab / Alt+A / Alt+O must work INSIDE inline editing

Current mechanics (all verified): dispatch is `handle_metadata_editor_key`
(keybindings.rs ~8704) matching `MetadataEditorPhase`; Alt+A/Alt+O arms
exist only in the `Editing` phase (~8794-8806); Tab in `Editing` cycles
rows (~8836); in `InlineEdit` (~8985), `DetailEdit` (~9247, the per-track
"<multiple values>" list), and `AddingKey` (~9167) all three fall through
to `handle_text_input_key` (text_input.rs ~521, no Tab arm) and are
swallowed. The user must press Enter first — that is the defect.

Wanted:
- Extract the two open-coded commit sequences into shared helpers —
  main-list commit (Enter body ~8999-9014:
  `metadata_editor_apply_inline_value_to_writable_slots` + clear input +
  phase + `recalc_dirty`) and detail commit (Enter body ~9261-9273) — the
  mouse handlers duplicate both today; everything below uses the helpers.
- Tab while inline-editing: commit the in-progress entry, advance to the
  next eligible field, and ENTER inline editing of it (BackTab reverse).
  In `DetailEdit` browsing mode, Tab/BackTab move between entries (today
  only Up/Down work); while detail-editing, Tab commits and advances into
  the next entry's editor (respecting
  `metadata_editor_detail_value_edit_refusal`). Design decision, yours to
  make and document: when Tab-advance in the main list reaches a
  `<multiple values>` row, hop into the detail overlay (Enter's behavior)
  or skip it.
- Alt+A while editing (any phase): commit the in-progress entry, then apply
  ONLY if dirty (`any_presentation_dirty` — the footer pill is already
  gated this way, draw_overlays.rs ~4608; make the keyboard path match).
  Alt+O: same commit, then the existing ok/close flow. In `AddingKey`, the
  sane pre-apply action is cancel (an uncommitted key name has no value).
- Do NOT rebind to Ctrl: Ctrl+A is readline cursor-home inside the text
  input by design (text_input.rs ~553-558).
- Sync the three pinned sites if footer pills change (draw list ~4600s,
  hit-test ~21580s, test tuple list ~30770-30790). Keep new logic in small
  named helpers (keybindings.rs is slated for a split refactor).

## P1-6. Browse: wide-glyph rows lose their right border (U+30FB class)

Field evidence: a row named `... {Japan Epic 25 ・ 8P-5137}` renders without
its right `|`. Ground truth: the character is U+30FB KATAKANA MIDDLE DOT —
East Asian Width Wide (2 cells), NOT ambiguous-width; the bug reproduces in
any terminal and with any kana/kanji/fullwidth character.

Mechanism (verified): Browse rows are literal-character compositions
(`render_entry_line`, draw_browse.rs ~1925-2016: left `│` + cells + right
`│`), but `pad_or_truncate` (~2163-2181) counts CHARS (`chars().count()`,
`take(width-1)`), so a wide glyph makes the row one cell too wide; the
"safety net" (~1988-1995) only pads shortfall, never trims overflow;
ratatui clips the overflowing last span — the right border. The July-8 fix
(a87ff7f) converted ONLY the tui-file-picker crate's `fit_text_*` helpers
to display columns; draw_browse.rs was never converted.

Wanted: centralize on ONE display-column width/truncate/pad helper (the
repo currently has FOUR independent implementations: picker
`render.rs ~1024-1100` — the best prior art with invariant tests
~1256-1310; `conversion_actions_ui.rs ~40-59`; the `Line::from(s).width()`
pattern in draw_source/draw_metadata; and the broken char-count family) and
sweep the char-count sites: draw_browse.rs `pad_or_truncate`/`truncate_to`
(~2339)/`truncate_left` (~1602)/filter row (~1835), inline_edit.rs
(~29, ~154), draw_overlays.rs `truncate_to_chars` (~50) + label math,
bookmarks_overlay.rs, recent_overlay.rs. Make the row safety net symmetric
(trim overflow too). Pin with a `render_entry_line`-width-equals-area test
over `・`/kanji/combining-mark names, modeled on the picker's invariant
matrix. Genuinely ambiguous-width characters (real `·`, `•`, `°`) are
internally consistent and OUT OF SCOPE.

---

# P2 — smaller audited defects (fix if the round has room; else report)

- `db.rs::copy_backup_over` (~2742-2790): `persist` over a symlinked
  DESTINATION replaces the link instead of restoring through it
  (contradicting its own comment), and the 0o600 temp drops the original's
  permissions. Restore through the resolved target and carry mode over.
- TUI probe DSD normalization gate (`probe.rs ~473`) matches
  `codec_name.starts_with("dsd_")` — DST-compressed DFF probes as `"dst"`
  and misses normalization/bit-depth-1 (pipeline classifier handles `dst`
  correctly, types.rs ~1704).
- DFF write attempts pay full-file backup + restore to fail (probe.rs
  ~7307-7344 order: backup before support check). Check support first.
- Editor multi-value lossiness: reads join distinct values with "; "
  (probe.rs ~5910, dsf_tags.rs ~26-36) and an edit writes the joined string
  back as one value; multiple COMM frames collapse (~808-817). At minimum,
  document; better, preserve unedited multi-values.
- GNUDB: multi-disc worker swallows per-disc errors (`if let Ok`,
  context_menu.rs ~1863) — total network failure reads as "no matches";
  `GnudbReview` Esc restores the parked editor without a session-guard
  match (keybindings.rs ~4979); `gnudb_operation_is_current` no-ops without
  restoring when an MB op is active (event_loop.rs ~5770, currently
  unreachable — pin the mutual exclusion or handle it).
- metadata.rs dual-read nit: an unparsable canonical value falls through to
  the legacy alias (first-parseable-wins, not canonical-shadowing —
  `find_map` with `.parse().ok()` inside, metadata.rs ~96-103; same pattern
  for DISCTOTAL ~113), contradicting the comment above it.
- Dead code from the dedup fork: `commit_batch`, `is_active_commit_item`
  (convert_actions.rs), `try_keychain` (keychain.rs ~508).
- Queue sanitize-rewrite (mod.rs ~2404) lacks fsync — it is the mechanism
  that scrubs legacy cleartext; a crash can resurrect the cleartext file.
- `convert_actions.rs` legacy carrier: hardcoded 24 for Source depth
  (~171) and raw sentinel 0 in `QualitySettings` (~178-183) — dead on the
  live path but a live trap; make the legacy carrier honest or unreachable.
- sacd-rs `dsdiff_dst_adapter_borrowed_path_has_no_per_frame_allocations`
  is flaky under parallel load (global allocation counter); serialize it or
  make the counter robust.
- macOS path divergence: `keychain_path` uses env `~/.config` while config
  uses `dirs::config_dir` (keychain.rs ~9-20) — unify when touching the
  secrets backend.

# Explicitly out of scope this round

- P1/companion-cue product decision: still OPEN with the user; do not
  change companion behavior.
- DFF tag WRITING (no standard chunk; read-side DST fix above is in scope).
- Ambiguous-EAW terminal-config handling (see P1-6).

# Delivery contract

Unchanged from the previous brief: complete files only, one archive, an
IMPLEMENTATION_REPORT listing every file touched and why, assumptions stated,
new behavior pinned with value-asserting tests, weakened tests justified.
Acceptance on our side: `cargo test --workspace` green — 4262 tests at HEAD,
of which 4261 pass and one is a known load-sensitive flake
(sacd-rs `dsdiff_dst_adapter_borrowed_path_has_no_per_frame_allocations`,
P2 item; passes in isolation) — zero cold-build warnings, and
`TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix` green. Verify the manifest first; if your copy of
the tree is incomplete, say so and scope down rather than guessing. If an
item can't be done to standard, ship the subset that can and say what you cut.
