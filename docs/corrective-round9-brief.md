# Corrective Round 9 — Pipeline Tag Reads, Naming Dots, Clipboard Gaps, Preset Disclosure

**Baseline:** branch `hardening` @ 7843058; `cargo test --workspace` =
5,322 passed / 0 failed across 56 targets. Version stays **0.4.4**.

**Field drivers (user, 2026-07-29):** all diagnosed against the live
Supertramp fixture `~/livetorrents/Supertramp – Even In The Quietest
Moments...-1977` (seven .wv files carrying a spec-invalid APEv2 key
`&год`, deliberately preserved by the round-8 writer). The user is
KEEPING that album broken as the acceptance fixture — after this round,
converting it as-is must produce ONE correctly named album folder with
correct tags and a disclosed warning. Every mechanism below was traced
to the line; do not re-derive.

---

## §1 Conversion pipeline: tolerant tag reads + loud degradation

### 1.1 The defect chain (verified end-to-end)

1. `materializer_single.rs:265-267` — the single-file materializer's
   metadata extraction:
   ```rust
   let tagged = match lofty::read_from_path(path) {
       Ok(tagged) => tagged,
       Err(_) => return Ok((TrackMetadata::default(), Vec::new())),
   };
   ```
   Any lofty failure (here: `Ape: APE tag item key contains invalid
   characters`) silently yields EMPTY metadata. No warning anywhere.
2. Template rendering (stages.rs:33398-33405): artist falls back to
   `"Unknown Artist"`, `%ALBUM%` falls back to the CONTAINER FILE STEM,
   year to empty — so each track rendered a distinct album folder
   ("Unknown Artist - A1 Give A Little Bit () [FLAC]" × 7).
3. Dispatch-time grouping (processor.rs:1120-1137) is provisional and
   keys on the source folder — it grouped correctly, but plan_outputs'
   per-track template dirs superseded it, fragmenting the album.
4. The only symptom in the log was the ReplayGain policy line
   ("inherited requested tag set was absent, incomplete, or
   unreadable", emitted at stages.rs:8900 when
   all_artifacts_have_complete_replaygain :8637-8657 finds the tag
   set incomplete on the staged OUTPUTS) — a downstream consequence
   of the same failed source read (outputs carry no tags because the
   source read yielded none).
5. Re-runs derive identical names → publish FailIfExists refuses
   against the previous run's folders (correct behavior; keep it).

### 1.2 Required: extract the tolerant reader to a shared home

The round-8 tolerant APEv2 machinery lives in `src/tui/probe.rs`
(native_ape_error_is_eligible :7323-7331, read_native_ape_tag
:7375-7526, native_ape_fields :7576+, read_native_ape_fallback
:7657+, the canonical seam read_canonical_metadata_file :7689-7716).
`src/convert` must not grow TUI coupling. NOTE (audit-corrected):
three disc materializers ALREADY import crate::tui
(materializer_bluray.rs, materializer_dvda_metabase.rs:17-18,
materializer_dvdv.rs) — pre-existing exceptions; do NOT add another,
and do not be confused by them. Recommended home:
`src/metadata_persistence.rs` — deliberately UI-neutral (module doc
line 3, imports only std + lofty), no dependency cycle. NOTE
(audit-corrected): src/convert does NOT consume metadata_persistence
today — this round introduces that import; do not hunt for an
existing one.

**THE SEAM DESIGN (audit-forced — a naive "move" is unimplementable):**
the parser core is container-agnostic and moves cleanly
(`read_native_ape_tag`, `NativeApeTag` :7301-7307 / `NativeApeItem` :7283-7289,
`ape_key_is_valid`, `u32_le_at`, `optional_id3v1_start`,
`display_escaped_ape_key`, `native_ape_error_is_eligible`,
`native_ape_canonical_key`, `native_ape_numbering_rows`, the
constants) — but "field extraction" and "fallback assembly"
(`native_ape_fields`, `read_native_ape_fallback`,
`read_canonical_metadata_file`) are BUILT FROM private TUI types
(`CanonicalEditorTagField` probe.rs:6660, `SourceMetadata` :46,
`MetadataReadIssue` :7799 — 54+ references across the TUI). Do NOT
drag those into the neutral module. Instead the shared layer exposes
a NEUTRAL outcome: rows of (raw_key: String, canonical_key: String,
item_key: lofty ItemKey, value: String, is_binary: bool) plus neutral
warnings (skipped-key disclosures as structured
{path, escaped_keys}). `src/tui/probe.rs` keeps thin wrappers that
rebuild `CanonicalEditorTagField`/`SourceMetadata`/
`MetadataReadIssue` from the neutral rows — every round-8 pin keeps
calling its existing probe.rs entry point and stays green. The §7
"wrappers, not rewrites" fence governs PIN CALL PATHS, not this seam
type — the neutral-row layer IS the sanctioned redesign.

### 1.3 Required: route the pipeline read sites through it

- **`materializer_single.rs:265` (primary):** on lofty failure, try
  the tolerant fallback; on fallback success, surface the recovered
  metadata PLUS a warning (the fn already returns
  `(TrackMetadata, Vec<warnings>)` — the second slot is the channel
  and it ALREADY reaches the log: warnings render in Per-Track
  Results via stages.rs:15946-15948 and reach the reporter via the
  helper at materializer_single.rs:225-250 — GENERALIZE that
  helper's hardcoded "DSF metadata warning" prefix, don't mislabel
  APE warnings); on fallback failure, return default metadata WITH a
  warning — never silently. **MAPPING SPEC (audit-forced):** the
  fallback's neutral rows must populate TrackMetadata the same way
  the happy path does — the named fields AND the provenance-marked
  `extra` map via the existing `item_key_to_extra_key` +
  `insert_source_text_tag` machinery (materializer_single.rs:285-296)
  — so the metadata write stage reproduces the FULL tag set on
  outputs (GENRE/COMMENT/DATE included) and the RG completeness
  check then sees real tags. Mapping only title/artist/album is a
  defect. NOTE: `processor.rs:755/:822`
  (single_file_batch_identity_probe) call the same
  read_track_metadata_with_warnings and inherit the fix
  automatically — no separate work; processor.rs also contains its
  own native APEv2 track-context parser (`apev2_track_context`,
  reachable from :1231-1247) — pre-existing precedent, leave it or
  converge it with a sentence of rationale in the report.
  Warning wording (model may improve): "tags unreadable for
  '<file>': <reason>; converting without metadata" / recovered case:
  "1 invalid APE key skipped in '<file>': '<key>'".
- **`queue_expansion.rs:1252-1268`** (`read_embedded_cuesheet_text_
  for_queue`): same silent `.ok()?` class — thread the fallback (an
  APE-failing file cannot carry a Vorbis CUESHEET, so the practical
  effect is nil for .wv, but the posture must match; cheap).
- **Sweep the remaining pipeline read sites** (audit-corrected
  inventory) and give each the same treatment or a written exemption
  in the engineering report:
  - `materializer_archive.rs:1806` — same silent pattern as the
    primary (its own read_track_metadata_with_warnings :1793):
    genuine target, same fix.
  - `materializer_cue.rs:835` (read_embedded_cuesheet, silent None
    on Err): genuine target. `:2757` already warns and degrades —
    verify, likely compliant as-is.
  - `replaygain.rs:53` — error-PROPAGATING (loud) and it is
    remove_stale_album_tags, NOT the RG inherit check (that check is
    stages.rs:8637 and is already loud). Tolerant reads help RG via
    the mapping spec above, not via this file. Exempt with that
    sentence.
  - `stages.rs:8647` — already loud; nothing to do.
  - `renaming.rs` — has ZERO lofty sites: it reads via the metaflac
    crate + subprocess (extract_metadata_from_flac, metadata.rs:61)
    with silent `if let Ok` drops at renaming.rs:102/:370,
    metadata.rs:145 — FLAC-only, dormant module; exempt with that
    classification, do not grep for lofty there.
- **TUI pane reader `src/tui/probe.rs` read_metadata (:588-615)** —
  the round-8 audit ledger item: browse panes/cue_generate/dr_report
  stringify the error with no fallback → empty pane metadata for the
  fixture album. Route it through the shared reader too.

### 1.4 Loud degradation contract (independent of APE)

Whenever conversion proceeds with unreadable/absent tags for a track:
- the conversion log's Per-Track Results carries an explicit
  "Tag read: FAILED (<reason>) — converted without metadata" line;
- the item's queue status/completion surfaces a warning count —
  SEAM (audit-forced): `ConversionStatus` (queue.rs:100-125) has no
  warning count on `Completed`; add a `#[serde(default)]` count
  field to `Completed` (serde-compat with the persisted queue JSON;
  the `CompletedWithActionErrors` variant :111-118 is the shape
  precedent) rather than a new variant;
- the log Settings section is unchanged.
This applies to ANY future unreadable-tag format, not just APE. Pins:
fixture-driven (construct an invalid-key .wv like the round-8 tests);
one pin proves the warning line appears in the built log text; one
proves recovered-fallback metadata reaches template rendering (folder
named from real ALBUM, not file stem).

### 1.5 Ordering-unprovable album batches: structural publish-root
sharing (pulled into scope — audit-forced, mechanism re-derived by
forced consensus)

MECHANISM, precisely (two audit passes disagreed; settled by direct
read at 7843058): the fixture's files carry NO TRACKNUMBER items and
`A1 …` filenames defeat `strict_track_number_from_dispatch_path`
(leading ASCII digit required, processor.rs:1197) →
`ambiguous_track_identity` → warn + `continue` at :466-474 —
album_batch is never assigned. BUT the warn deliberately "leav[es]
legacy completion-order conversion.log append enabled": the
incremental escape `is_incremental_single_audio_publish`
(stages.rs:19771-19807) fires on `has_fragment || (single audio &&
standalone conversion.log)`, and independent jobs with
`write_conversion_log` on DO carry a standalone log sidecar
(stages.rs:9200-9217). So under DEFAULT settings (logs on) the seven
converged jobs append into ONE shared folder via the legacy escape.
The HARD-FAIL class (the Mazzy Star field record) materializes when
the escape is dead: `suppress_incremental_conversion_log_append` set
(the settings-mismatch strip
`mark_queued_album_batch_as_ordering_unavailable` processor.rs:1181,
and the dispatch-preparation-failure path :589) or conversion logs
disabled. All seven fixture tracks also get
`track_number = unwrap_or(1)` (materializer_single.rs:72) — the
completion-order log is the only ordering record.

REQUIRED (fix shape (b), hardened to the re-derived mechanism):
shared-publish-root behavior for ordering-unprovable batches must be
STRUCTURAL, not an accident of log settings — ordering-unprovable
groups get album_batch membership with completion-order logging and a
disclosed "track ordering unavailable; logged in completion order"
warning, and the same treatment covers the suppress paths
(settings-mismatch :1181, dispatch-prep failure :589) and logs-off.
COMPOSITION CONSTRAINT (audit-flagged): album_batch fragment
machinery keys ordered logs on track numbers, and this fixture's
tracks are all number-1 — specify that unprovable-ordering membership
uses completion-order fragments/append (the legacy semantics), never
number-keyed ordering; ordered logging stays exclusive to proven
ordering. During implementation, RE-DERIVE the Mazzy Star record
(memory: bug-vinyl-tracknumber-publish-collision) against this
mechanism and state in the report which leg (suppress vs logs-off vs
other) produced the field failure.
Pin family: ordering-unprovable multi-file single-run batch publishes
ALL tracks into one album dir under (a) logs ON — may already pass
via the legacy escape: pin it so it can never regress; (b) logs OFF;
(c) the settings-mismatch suppress path; (d) the Mazzy shape
(A1/B2-style TRACKNUMBER tags present). Vinyl side-number PARSING
(fix shape (a)) stays FENCED.

### 1.6 Acceptance (the user's fixture, run manually post-round)

Converting the Supertramp folder with a 192k preset must produce ONE
folder "Supertramp - Even in the Quietest Moments... (1977) [FLAC]
{US A&M SP-4634 LP  32-192}" (dots intact per §2; the album tag's
trailing parenthetical routes to {TITLE_EXTRA} via
looks_like_title_extra_metadata — audit-verified), ALL SEVEN tracks
published into it (per §1.5), correct per-track titles, disclosed
invalid-APE-key + completion-order warnings in the log.

---

## §2 Naming: stop eating dots

### 2.1 Mechanism (verified; no test pins the current behavior)

`sanitize_component()` stages.rs:35252-35272 runs `.trim_matches('.')`
(line 35265) on EVERY template token — bidirectional, so
"...And Then There Were Three..." loses both runs in one call.
Redundant back-up eaters: `path_from_template_components`
stages.rs:34997 (per final path component — also eats literal dots
typed in templates), `sanitize_title_extra_component` stages.rs:35301,
`sanitize_album_batch_component` processor.rs:1672. No comment
explains any of them; interior dots survive; Unicode `…` survives
(not U+002E). NOTE the existing fork: tag-based renaming
(`sanitize_for_filesystem` renaming.rs:195-216) PRESERVES edge dots —
conversion naming is the outlier; this round converges on the
renaming path's semantics.

### 2.2 Required changes

- Drop `trim_matches('.')` at all four sites. Replace with the only
  genuinely dangerous guard: a component that is exactly `.` or `..`
  → the existing `"untitled"` fallback idiom (stages.rs:35267-35271)
  / `"Album"` for the batch component. `reject_escaping_path`
  (stages.rs:35310-35323) stays the traversal backstop; the
  empty-component filter at :34998 stays.
- `path_from_template_components`: replace the per-component trim
  with the exact `.`/`..` check (mirror src/tui/rename_plan.rs:68 —
  the TUI file, NOT src/convert/rename_plan.rs which has no such
  check).
- **EXTENSION-JOIN HAZARD (audit-found, must fix):**
  `append_default_extension` (stages.rs:35404-35419) uses
  `path.set_extension(ext)` — for a stem ending in "...",
  `extension()` is `Some("")` and set_extension CONSUMES the last dot
  ("01 - Moments..." + flac → "01 - Moments...flac", one dot eaten).
  Replace with a manual "." + ext push on the file name, and
  re-derive the extension-already-present check (:35410-35414, which
  also misreads dot-terminated stems). The collision-suffix helper
  (:35421-35431) is verified safe. PIN: trailing-dot title
  round-trips through extension append without dot loss.
- The conversion-actions path inherits automatically via the
  `ComponentSanitizer` pointer seam (stages.rs:34303-34305,
  actions.rs:436/17017, stages.rs:20765,
  conversion_actions_ui.rs:2372) — verify with a pin, change nothing
  there. NOTE the pointer type `fn(&str) -> String` (actions.rs:427)
  cannot carry a flag — see the windows_portable placement below.
- **Windows-portable mode (opt-in, config):** `[naming]
  windows_portable = false` (default). PLACEMENT (audit-forced): no
  [naming] config section exists yet (TonepoetConfig,
  src/config.rs:24-34) — add it; carry the flag on `NamingPolicy`
  (src/convert/pipeline/types.rs:831-836, `#[serde(default)]` bool;
  PRODUCTION construction sites (audit-corrected): src/main.rs:1908,
  src/convert/mod.rs:3044,
  src/convert/pipeline/unified_request.rs:136 (the request builder on
  the MAIN queue path — do not miss it), src/tui/command.rs:8795.
  processor.rs:3572 is a TEST fixture, and ~14 more test-module
  NamingPolicy struct literals need the new field set explicitly
  (serde defaults do not help struct literals). Apply the
  strip at FINAL ASSEMBLY (`path_from_template_components` + the
  album-dir/batch joins) where `req.naming` is in scope — NOT inside
  the four sanitizers (they're flag-free free fns and the
  ComponentSanitizer fn-pointer cannot capture the flag). When true:
  strip trailing dots/spaces per component — trailing only, never
  leading. No `…` substitution this round (lossy vs the tag; the
  renaming path keeps literal dots — do not fork).
- DISCLOSE (report + doc line): a leading-dot album folder is a
  hidden directory on Unix — deliberate; the user asked for canonical
  titles. Include that tonepoet's OWN Browse screen hides it too when
  show_hidden is off.
- Reserved-char behavior UNCHANGED this round (`?`/`"` → space —
  "Who's Next?" still loses its mark; recorded as a possible future
  substitution-table alignment with renaming.rs, NOT in scope).

### 2.3 Pins

Round-trip pins for: trailing-run album ("Even in the Quietest
Moments..." → folder keeps dots), leading+trailing ("...And Then
There Were Three..." intact), bare `.`/`..` component → fallback,
template-literal dots survive assembly, TRACK FILENAME trailing-dot +
extension join without dot loss (the set_extension hazard),
windows_portable=true strips trailing only, batch component parity,
ComponentSanitizer parity. Existing harness pattern:
stages.rs:41168-41320 template tests; processor.rs mod tests :3395.

---

## §3 Clipboard: the four real defects (round-8 research truth)

Research REFUTED the "separate app text-input machinery" hypothesis:
`src/tui/text_input.rs:1-3` is a pure re-export of the picker crate
engine; every inline editor's Ctrl+C/X already publishes through the
hooked shared clipboard → OSC 52. The field failures decompose into:

- **3.1 Info-pane Ctrl+C interception (defect).** Ctrl+C on a focused
  (non-editing) info-pane field is consumed by the Browse-global
  FILESYSTEM-clipboard arm (keybindings.rs:5609-5613) before info
  dispatch (:5651-5658) — it copies the file, not the field text.
  Give the info-focus state first refusal on Ctrl+C: when
  `app.browse_info_focus` is Some (app.rs:10986,
  `BrowseInfoFocus::Metadata(field)` :9896-9898), publish the field
  VALUE via `publish_text_clipboard` — the accessor EXISTS:
  `current_browse_metadata_value(app, field)`
  (keybindings.rs:3315-3328, the same seed the inline editor uses).
  Info focus and files-list ownership are mutually exclusive in state
  (`files_navigation_active()` asserted :5668-5671), so the guard on
  the 'c' case cannot break file-list copy. Statuses:
  "Copied: <value>" / empty-or-unprobed field → "field is empty;
  nothing copied". DELIBERATE SCOPE: only Ctrl+C changes — Ctrl+X/V/P
  keep filesystem semantics while info-focused (cut/paste of a
  read-only info field is meaningless; copy was the reported gap).
  State this in the report.
- **3.2 Convert-screen view-mode copy (gap).** No copy gesture exists
  outside an active inline edit (`handle_convert_key` keybindings.rs
  :625+ has no copy arm — verified through ~:955). PER-PANE SPEC
  (audit-forced — there is NO uniform focused-field model):
  - Metadata pane, inline-fields mode (single/archive-preview —
    `field_focus: ConvertMetadataField`, app.rs:4995): Ctrl+C copies
    the focused field's value. Batch/MultiTrack modes have a
    file-list cursor, not fields → NO copy arm; disclose.
  - Format pane (`field_focus: FormatField`, app.rs:3400 — pill
    rows): Ctrl+C copies the focused row's SELECTED OPTION LABEL.
  - Output-options pane (`field_focus: OutputOptionsField`,
    app.rs:5036): text rows copy the value; pill rows the label.
  - Source pane: NO field-focus model exists (SourceState,
    app.rs:2972-2993) → NO copy arm this round; disclose.
  All copies dual-plane with "Copied: <value>" status; scoped to the
  Convert screen.
- **3.3 Copy-without-selection silent no-op (defect).**
  `copy_selection` returns false on no selection
  (text_input.rs:272-274) and every call site discards it. Rule and SEAM SPLIT
  (audit-forced — the engine has no status channel): BEHAVIOR lives
  once in the engine's Ctrl+C arm (text_input.rs:925-935 region): no
  selection → copy the WHOLE field value (mc/readline convention),
  return true; Ctrl+X with no selection keeps refusing (returns
  false). STATUSES live app-side at a SCOPED set of call sites only —
  the metadata-editor edit arms (keybindings.rs:13445-13449 region),
  the browse inline-edit and path-bar arms, and the new §3.1/§3.2
  arms — "Copied entire field" / "nothing selected; nothing cut". Do
  NOT touch all ~80 dispatch sites; the engine's bool suffices.
  Verified harmless side effect: picker search restart_search on
  true (input.rs:718) already fires on copy today.
- **3.4 Paste staleness (defect).** `paste_clipboard`
  (text_input.rs:295-301) prefers the field's stale LOCAL clipboard
  over the shared one. Rule: shared clipboard wins whenever
  non-empty; the per-field plane becomes fallback-only (or is
  removed if nothing depends on divergence — model's choice, state
  it). Pin: copy in field A then paste in field B that copied
  earlier → A's text.
- **3.5 Delivery docs.** The dominant field cause is byobu/tmux
  swallowing OSC 52 (`allow-passthrough` defaults OFF in tmux ≥3.3;
  `set-clipboard`; `Ms` under screen-256color). `:help clipboard`
  exists (command.rs:4355-4390) and ALREADY carries the exact tmux
  commands (:4373-4374) — the only additions are a "test it" hint
  (copy something, paste outside) and the `Ms`-capability /
  TERM=screen-256color note. NO external-tool fallback this round
  (wl-copy/xclip target the remote display over SSH — useless for
  the user's topology).

Pins: info-pane field copy (both planes, correct precedence vs
filesystem arm); convert view-mode copy; whole-field copy on empty
selection + statuses; paste-prefers-shared.

---

## §4 Read-issues display + invalid-key repair

- **4.1 Collapse identical per-file issues.** The metadata overlay's
  Read issues panel repeats the full path per file, truncating the
  informative tail. When ≥2 files in the open set share an identical
  issue signature (same kind + same reason-with-path-elided), render
  ONE line: "7 files: 1 invalid APE key skipped: '&год'" — MECHANISM
  (audit-corrected): there is NO issue-row drill — issue rows are
  static Lines in scrollable overlay sections
  (draw_overlays.rs:6138-6149); do not invent one. The grounded
  implementation: elide the path from per-row reason text (the
  MetadataIssue variants carry a structured `path`, app.rs:6461-6469
  — mechanical string surgery in the view model,
  metadata_view_models.rs:900-960) AND collapse identical
  path-elided signatures across files into the one summary line.
  NOTE: the typed `MetadataReadIssueKind` is string-erased before
  the view model (keybindings.rs:18454) — thread the kind (or a
  recoverable flag) into editor state; §4.2's enablement predicate
  needs the same threading.
- **4.2 Per-album repair action.** Editor context (and/or tags popup)
  gains "Remove invalid APE key(s)" — enabled only when the open
  set has RecoverableTagWarning issues naming skipped keys. MECHANISM
  (audit-forced — a changes-list CANNOT express this: invalid keys
  are unnameable as lofty ItemKeys): add a NEW public repair entry
  point — a drop-invalid-items flag threaded through
  `prepare_native_ape_replacement` (probe.rs:9144-9242; the
  unconditional key-less preserve is at :9182-9185) and a thin
  public fn REUSING `write_all_tags_native_wavpack_ape_atomic`'s
  machinery (:9276-9363: wvpk check, journal assert, snapshot
  guards, cancel points, atomic temp+rename) — do NOT duplicate
  that body. Blocking confirm names files and keys — enumerate keys
  via a cheap per-file `read_native_ape_tag` re-parse (NOT by
  parsing warning-reason text); enablement via the §4.1 typed-kind
  threading. Per-file results; Strong/Standard verification honored. After
  repair, lofty reads natively → the warning disappears and healthy
  files return to the Lofty write route (recovery-only predicate,
  probe.rs:9265-9274 — verify this transition with a pin).
  FENCED: the Utilities-menu scan-at-scale tool stays parked
  ([backlog: flac-id3-prefix-tooling] gets a sibling entry).

---

## §5 Preset save disclosure

Mechanism (verified): the rate pill's first option is
`(SOURCE_SAMPLE_RATE_SENTINEL = 0, "source")` (app.rs:3365,
:3562-3567); preset save captures the pill VALUE (presets.rs:355) —
saving while on "source" stores mode-keep, which the user read as
"192" because their then-loaded source was 192k. Field result: both
"-192"-named presets carry `sample_rate = 0` and convert 384k
sources at 384k.

- **5.1 Save-time disclosure.** Build the resolved-semantics summary
  ONCE (a helper on TuiPreset/FormatState) and append it at EVERY
  save confirmation — audit-counted ~6 sites: command.rs `:saveas`
  :4309-4313, `:w` :4057-4061, second write path :4101-4109 (NOTE: a SILENT save today — no
  set_status at all; add a status line there for the summary to land
  on),
  event_loop.rs:1272-1278 (picker save-as completion),
  keybindings.rs:5139 and :38420-38426. Wording: "rate: keep source
  · depth: 32-bit · dither: tpdf". "keep source" is spelled out for
  the MODE captures only — rate sentinel 0 and bit_depth "source".
  AUDIT-CORRECTED: dither "none" is an EXPLICIT off-switch
  (DitherType::None app.rs:3599; dither_applies stages.rs:16888
  returns false unconditionally) — render it literally
  ("dither: none"), never as "keep source".
- **5.2 Display surface (audit-forced landing):** there is NO
  per-preset display surface today (`:presets` renders names only
  into one status line, command.rs:4319-4326). Land the same
  resolved summary on the LOAD confirmation ("Loaded preset: …",
  command.rs:4288-4292) — the only honest existing fit. Do not
  invent a screen; do not stuff per-preset details into the
  `:presets` one-liner.
- NO behavior change to capture semantics (mode-capture is
  legitimate and useful). NO name-vs-settings heuristics.
- Pins: saving with pill on "source" produces the disclosure wording;
  saving with explicit 192000 shows "192 kHz".

---

## §6 Round-8 ledger items folded in (small, mechanical)

1. **Hermeticity:** the five DB-touching probe tests take
   `XdgConfigHomeGuard` (test_support.rs:38-82) like the
   mixed-transfer test (tag_interchange.rs:927-933, same rationale
   comment). Audit-verified names/lines: …preserves_invalid_ape_item
   :11896, …empty_string_deletes :11931, …empty_numbering_deletes
   :11967, …byte_idempotent :12050, inline_wavpack_dispatch :13754.
2. **APE parser allocation cap:** `Vec::with_capacity(item_count)`
   pre-allocates before item parses (probe.rs read path) — clamp to
   `min(item_count, region_len / 9)` (9 = minimum item encoding) so a
   lying footer cannot transiently allocate ~100 MB.
3. **Stale-session suffix guard:** the first-of-N and
   ignored-directories suffixes (event_loop.rs:873-888, :898-914,
   both invoked at :5237-5238 in the FilePickerComplete arm) append
   even after the reducer discarded a stale completion.
   AUDIT-DECIDED SHAPE: `reduce_file_picker_complete` returns
   `consumed: bool` and the two appends are gated on it — the
   reducer's ~11 purpose arms have DIFFERENT stale predicates
   against different holders, so the call site cannot mirror the
   check; do not attempt the alternative.

## §7 Fences (unchanged this round)

Pairing-guard exact-sequence relaxation (await user field evidence
from round-8 multi-FILE transfers); vinyl-style side-number PARSING
(collision fix shape (a) — §1.5 ships shape (b) only);
reserved-char substitution table
for naming; `…` substitution; mount-capability-aware naming; external
clipboard tools; Utilities scan-at-scale repair tooling; Custom
builder + Paste tags (still next feature round); config cascade;
library. NO F-keys; NO emoji/decorative unicode; Ctrl+Q stays quit;
new bindings scoped to their screens (Convert-screen Ctrl+C copy arm
scoped to Convert; info-pane arm scoped to Browse info focus);
version 0.4.4; never truncate gate output; rounds 5-8 machinery must
not regress (round-8 pins must stay green through the probe.rs
extraction — wrappers, not rewrites).

## §8 Deliverables

Overlay bundle (tar.gz, nested dir, preimage manifest with SHA-256 of
exact base revisions) + engineering report with: per-item named pins
(minimum: §1.4, §1.5, §2.3, §3, §4.2 transition, §5, §6 items), the shared
extraction's module map (what moved to metadata_persistence.rs, what
wrapped in probe.rs), disclosed limitations (leading-dot hidden dirs;
reserved-char class unchanged; repair is per-open-set not at-scale),
and any deviation with rationale. `cargo test --workspace` green
against 5,322/0; new tests must FAIL if the behavior they pin
regresses.
