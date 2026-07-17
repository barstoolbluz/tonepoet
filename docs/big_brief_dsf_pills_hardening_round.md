# Big brief: DSF tagging, source-coupled pills, ReplayGain policy, GNUDB/editor hardening, secrets

HEAD at time of writing: `afffe61` (branch `working`). A full-tree archive
accompanies this brief; it is cut at the follow-up commit that ADDS this
brief (afffe61 + this document), so treat the archived tree as HEAD. You are working WITHOUT a compiler: every file you
deliver must be COMPLETE (no elisions, no "rest unchanged" markers), and the
applying side will fix mechanical compile errors only — logic must be right
on delivery.

## Engineering standards — read first

We are aiming for the **highest engineering standards** in everything below.
Concretely, that means:

- Fail closed, never open, on ambiguity: an unreadable input, a missing fact,
  or an unclassifiable state degrades to refusal or a labelled placeholder,
  never to a silent guess. (Recent precedent: merged-cue INDEX emission
  degrades to placeholder zeros when boundary facts are incomplete rather
  than dividing by a guessed rate — `stages.rs::build_cue_sheet`.)
- Structured signals over string matching: cross-layer decisions ride typed
  fields (`SplitCueFolderRejection.references_in_folder_audio`), not
  warning-text parsing.
- Every behavioral claim gets a pin: a test that FAILS if the behavior
  regresses, asserting values, not presence. Watch for self-satisfying
  sentinels (a known weakness class in this repo — see H3.3).
- Honest degradation is user-visible: if you bound coverage or skip work,
  log or surface it; silent truncation reads as "done" when it isn't.
- No new `unsafe`, no new panics on user-controlled input, subprocess stdin
  always `Stdio::null()` (established repo invariants).

## R — Review our recent local rounds and fix what you find

Before or alongside the feature work, review commits `7eb466e`, `6a56090`,
`bdb0a43`, and `afffe61` (all local-fix rounds done without you). Treat them
adversarially: if you find weaknesses, shortcomings, or cleaner designs, fix
them. Our own audit already flagged the following residuals — they are YOURS
to fix in this round:

- **R1. Alias sweep ordering (probe.rs `apply_comment_changes`)**: each
  change retains-out ALL alias spellings (via `vorbis_key_aliases`) then
  pushes its own value. Two changes touching the same alias group in one
  save (e.g. a COMMENT edit + a DESCRIPTION delete) can destroy the earlier
  edit depending on entry order. Also: editing COMMENT deletes a genuinely
  distinct DESCRIPTION value with no warning, and the still-open editor rows
  are not refreshed. Decide and implement a coherent policy (e.g. resolve
  alias groups BEFORE applying, or merge rows at display time so the
  conflict cannot arise — see R3).
- **R2. Alias coverage is FLAC-native-path only**: Opus/Ogg go through
  `write_all_tags_lofty_with_backup` (plain `remove_key`), so the
  stale-duplicate-spelling bug fixed for FLAC persists on other Vorbis
  carriers. Extend the alias-complete write behavior to all Vorbis-comment
  formats.
- **R3. Duplicate editor rows for alias-tagged files**:
  `ensure_standard_fields_present` (probe.rs) matches RAW display keys, so a
  file tagged only DESCRIPTION shows a DESCRIPTION row plus a synthesized
  empty COMMENT row (both canonicalize to COMMENT). Merge alias groups at
  read/display time; this is the enabler for R1's conflict.
- **R4. Dual-read preference inconsistency (src/convert/metadata.rs)**: on
  conflicting spellings, legacy TOTALTRACKS wins for track totals (metaflac
  `vorbis.total_tracks()` reads TOTALTRACKS first) but new DISCTOTAL wins
  for disc totals. Make both prefer the canonical new spelling
  (TRACKTOTAL/DISCTOTAL) deterministically.
- **R5. TUI DSD byte-rate display and cascade (src/tui/probe.rs:419,
  src/tui/app.rs `cascade_dsd_source_to_pcm_defaults`)**: ffmpeg-next
  reports dsd_u8 BYTE rates (bit rate / 8), the same disease fixed in the
  pipeline materializers via `normalize_dsd_probe_rate`
  (src/convert/pipeline/types.rs). The TUI never normalizes: a DSD64 DSF
  displays "352.8 kHz" instead of "DSD64 (2.8 MHz)", and the DSD→PCM
  defaults cascade early-returns (`DsdRate::from_hz(352_800)` = None) so
  file-based DSD sources never get the recommended rate/Int24/sox defaults
  (only SACD ISOs do, whose synthesized probe uses the
  `SACD_SAMPLE_RATE_HZ = 2_822_400` constant from src/tui/sacd.rs).
  Normalize at the TUI probe boundary; reuse the existing helper.
- **R6. DSF header override trusts corrupt headers
  (materializer_single.rs / materializer_archive.rs)**: the container-header
  fact override applies on any `Ok(Some(_))`, even when the DSD inspector
  flagged validation errors or reports `sample_count_per_channel == 0` — a
  corrupt header replaces a workable ffprobe estimate with a guaranteed
  post-encode drift failure. Gate on validation status and nonzero counts.
- **R7. TMPDIR-sensitive synthetic-artifact check
  (queue_expansion.rs `is_synthetic_cue_album_artifact`)**: the path-shape
  check compares against `std::env::temp_dir()` at CHECK time; a persisted
  queue item resumed under a different TMPDIR fails the check and the
  synthetic planner `album.cue` is swept into the published album again
  (and `cleanup_synthetic_cue_artifact` silently no-ops). Make the check
  robust to TMPDIR drift (e.g. match the invariant path SHAPE
  `…/process-*/artifact-*/album.cue` + marker filename, or persist the
  synthetic root with the request).
- **R8. No unit pin on the synthetic companion branch
  (stages.rs `companion_source_dir`)**: the branch that routes synthetic
  cue-album jobs to `companion_prepared_track_common_source_root` is pinned
  by nothing mechanical — add a test constructing a synthetic-shaped request
  and asserting the temp artifact dir is never chosen.
- **R9. Generated cue values are unsanitized (stages.rs
  `build_cue_sheet`)**: CATALOG/ISRC are emitted unquoted (arbitrary tag
  strings — whitespace breaks the statement); embedded `"` in
  TITLE/PERFORMER/GENRE is unescaped; embedded newlines corrupt the sheet.
  Sanitize/validate (e.g. drop CATALOG unless it matches the 13-digit
  UPC/EAN shape; strip newlines; escape or strip quotes).
- **R10. BD/DVD-V editor vocabularies not flipped to the new tag canon
  (src/tui/command.rs)**: `bluray_is_album_level_sidecar_key` and the DVD-V
  editor TOML key table still speak only TOTALTRACKS — a value typed into
  the new TRACKTOTAL core row saves as an unclassified custom key. Teach
  both vocabularies the canonical spellings (keep legacy accepted).
- **R11. Dead entries in probe.rs `STANDARD_KEY_ORDER`** ("YEAR",
  "TOTALTRACKS", "TOTALDISCS" can never match post-canonicalization) — prune.

## F1 — DSF tagging (the big one: 100s of DSD albums are untaggable)

**Broken today**: right-click → Edit metadata on a folder of `.dsf` files
fails with lofty's "No format could be determined" (empirically observed;
a screenshot exists on our side — not in the archive, take the failure as
established fact). lofty 0.21 has no DSF support. DSF files carry ID3v2
chunks at a header-declared offset.

**What we want**:
- Read AND write ID3v2 tags in `.dsf` files. The `id3` crate (established,
  pure Rust) is believed to support DSF natively
  (`id3::Tag::read_from_dsf_path` / `write_to_dsf_path` in id3 ≥ 1.x) —
  this API claim is NEEDS-VERIFICATION (the crate is not currently a
  dependency, so it isn't in the offline registry; the applying side will
  verify the exact API at apply time and fix signature mismatches). Design
  so the DSF I/O surface is a thin, swappable seam. A hand-rolled DSF chunk
  walker is also acceptable if you judge the dependency unjustified, but do
  not reimplement ID3v2 itself.
- Route DSF through the SAME editor surfaces as other formats: the metadata
  overlay (read_all_tags_merged path in probe.rs), single-file and
  multi-file editing, save with backup semantics matching
  `write_all_tags_lofty_with_backup`.
- Metadata provenance must carry through to the Convert screen's metadata
  pane (`src/tui/draw_metadata.rs`, populated via `read_metadata` in
  probe.rs) and to the naming-template tokens (%ARTIST%, %ALBUM%, etc.) used
  for folder/file naming — a DSF album must rename-from-tags exactly like a
  FLAC album.
- Map ID3v2 frames to the editor's canonical display keys consistently with
  the Vorbis canon (TRACKTOTAL/DISCTOTAL/COMMENT display canon; ID3 stores
  totals inside TRCK/TPOS "N/T" — the mapping layer owns that translation).
- DFF (DSDIFF) tagging is out of scope unless trivially free (DFF has no
  standard tag chunk; do not invent one).

## F2 — "Same as source" pills (preset decoupling)

Add a `same as source` option as the FIRST pill on BOTH the `bit depth` and
`sample rate` rows of the Format pane (src/tui/app.rs `FormatState`:
`sample_rate: PillState<u32>` at ~2804, `bit_depth: PillState<BitDepthChoice>`
at ~2805; rendering in src/tui/draw_output.rs).

Why: presets currently hard-couple to specific rate/depth values; a
"same as source" preset converts any input without carrying a fixed target.

Notes:
- The pipeline already has Source semantics end-to-end (`Source` rate/depth
  resolution in tonepoet-pipeline plan.rs, including the DSD dynamic
  defaults `DsdRate::default_pcm_target_hz` and the depth-honesty matrix).
  This is primarily TUI/preset surface work: pill options, constraint
  cascade (`apply_format_constraints` at app.rs ~3619 — think through how
  per-format caps interact with a source-relative choice), preset
  serialization (a preset saved with same-as-source must round-trip), and
  wizard_integration mapping to `ConversionOptions`.
- `sample_rate` is `PillState<u32>` — same-as-source needs a sentinel or a
  type change; prefer the honest type change over a magic number if the
  blast radius is acceptable; otherwise document the sentinel at its
  definition.
- Display: the pill label should read `source` and the resolved value may be
  shown contextually when a source is loaded (e.g. "source (→ 352.8k)") —
  your call on polish, but do not fabricate a resolution when no source is
  loaded.

## F3 — ReplayGain: skip when tags already present

Today the ReplayGain stage (loudgain, stages.rs ~6749) unconditionally
rescans. Most of the user's 20k+ albums already carry RG tags.

Wanted: if RG tags already exist on ALL output tracks, skip scanning unless
the user explicitly opts into rescanning. Expose the policy as an additional
option on the ReplayGain pill row (`ReplayGainChoice` in src/tui/app.rs ~262:
Off/Album/Track/Both — add a skip-if-present flavor or an orthogonal
modifier; design the UX so the default preserves current explicit behavior
and presets round-trip). Decide what "present" means precisely (which tags,
album vs track gain, all-or-any) and document it. CLI: extend `--replaygain`
correspondingly (src/main.rs).

## H1 — GNUDB: keep it, harden it (+ identityless completions)

User decision: GNUDB stays, but the flow must be made safe. Known defects
(docs/known_issues_after_g_round.md items 4 and 6, evidence at 7eb466e):

- GNUDB completions gate identity but still REPLACE whatever overlay is open
  (event_loop.rs ~4186/4220/4249 at HEAD: GnudbSelect, GnudbReview,
  multi-disc review — all direct `app.active_overlay =` assignments): a slow
  query completing over a dirty editor destroys it. Park/restore like the MB
  flow, honoring overlay authority.
- GNUDB workers have no panic wrapper and no cancel command: a wedged worker
  holds authority until a fresh `:tags-mb` silently retires it; a gnudb read
  failure strands a parked dirty editor invisibly while the quit gate blocks
  on it (event_loop.rs ~4223). Add panic containment, a cancel path, and
  user-visible retirement.
- **Identityless completions** (same class):
  `MetadataEditorSplitCueAlbumGroupingComplete` installs its editor
  unconditionally, even on Err (message.rs:606; handler at
  keybindings.rs ~10944 — the Err arm builds an AmbiguousMerge fallback and
  still installs), replacing live pickers / cancelling live MB operations.
  Same for `CuePreviewComplete`/`CueMbComplete`/`CueFillComplete`
  (message.rs ~529-565; event_loop handlers ~4384/4434/4451, no
  operation-id gating). Give all of these the same identity/authority
  gating the MB completions received.

## H2 — Archive password secrets: Design B (keychain reference)

User decision: **Design B**. Archive passwords currently live cleartext in
TWO stores: `config.toml` (`archive_password: Option<String>`,
src/config.rs:~319) and the MRU plaintext password list at
`~/.config/tonepoet/passwords.toml` (src/tui/keychain.rs — 0600, its own
doc comment says "NOT a security vault"). Additionally there is a CONFIRMED
third leak: passwords persist into `conversion_queue.json` —
`ConversionItem.archive_password: Option<String>` (queue.rs:~178, plain
Serialize) and `PipelineRequest.source.archive_password: Option<SecretString>`
where `SecretString` derives plain `Serialize` (Debug/Display are redacted
but Serialize is plain; src/convert/pipeline/types.rs:~28). Replace all of
this with an OS keychain/secret-service REFERENCE:

- Store secrets via the freedesktop Secret Service (Linux; `secret-service`
  or `keyring` crate — prefer `keyring` for cross-platform reach since the
  project targets macOS too) under a stable service/account naming scheme;
  config stores only the reference key. Like F1's id3 note, the keyring
  crate API is NEEDS-VERIFICATION (not a current dependency, so not in the
  offline registry): keep the secret-store surface a thin, swappable seam
  and the applying side will fix signature mismatches at apply time.
- Migration: on load, if cleartext is found in `config.toml` or
  `passwords.toml`, do a one-time migration (store to keychain, rewrite the
  file without the cleartext, back up the old file).
- Fix the queue leak: passwords must not serialize into
  `conversion_queue.json` — give `SecretString` a redacting/skipping
  `Serialize` (or `#[serde(skip)]` + rehydrate-from-reference on load) and
  do the same for `ConversionItem.archive_password`. Decide and document
  what a resumed queue item does when its secret is no longer available
  (fail closed with a clear re-prompt path).
- Fallback: headless/no-keyring environments must degrade gracefully
  (explicit error naming the missing backend; an opt-in env var for CI is
  acceptable) — never silently store cleartext again.
- CLI `--archive-password` remains as an ephemeral override (never
  persisted) — verify it cannot reach logs or any serialized artifact.

## H3 — Known-issues backlog (docs/known_issues_after_g_round.md, items 10–13)

Items 1–3, 5, and 7–9 were fixed locally (6a56090). Remaining, evidence in
the doc (line refs are at 7eb466e — re-verify against HEAD):

- **H3.1 (item 10)** wvunpack preflight runs after materialization and
  pre-actions, contradicting its "before expensive work" doc; serial path
  untested.
- **H3.2 (item 11)** lossy-CUE residuals: the 8192-sample shortfall floor
  makes the truncation guard vacuous for tails under ~186 ms; boundary
  ADMISSION still trusts header duration (an understating Xing-less VBR
  header can reject a legitimate final track before LossyTail measures);
  interior truncation errors name the `.tmp` path instead of the image.
- **H3.3 (item 12)** weakened/self-satisfying sentinels: `gnudb_back` pin is
  variant-only; `custom_format_sentinel` no longer carries
  `AudioFormat::Custom`; four command.rs self-scan sentinels have
  EOF-unbounded windows satisfied by their own literals. Restore real teeth.
- **H3.4 (item 13 residuals + audit leftovers)**: dead `Unspecified` arm
  (tonepoet-pipeline/src/plan.rs:~1573, now `Unknown | Unspecified`);
  prefetch dead on the compat `ConfirmAction::MbBack` picker; README
  20-bit claim in the wrong crate; S11 retry gap and the quit gate
  blocking on in-flight parked editors (both defined in
  docs/residual_apply_audit_brief.md — S11 and S1 — included in the
  archive). Item 13's duplicated-fixture-line and stale-marker nits and
  item 8's `TONEPOET_REQUIRE_TOOLS` split were already fixed at 6a56090 —
  do not hunt for them.

## P1 — OPEN product decision: companion `.cue` for per-track outputs

When a cue-image album is converted to per-track files, the REAL source
folder's `.cue` sheets are now swept as companions (post-`bdb0a43`). Their
FILE refs point at images that are NOT in the output (e.g. `tdsotm_a.wv`).
Options: (a) keep sweeping (provenance value, mild confusion), (b) suppress
source cues for Tracks-layout outputs, (c) rewrite swept cues against the
generated per-track cue. We deliberately did NOT decide. Present a
recommendation with tradeoffs in your implementation report; implement it
ONLY if (b) — the cheapest reversible option — or leave as-is and say why.

## Delivery contract

- Complete files only, one archive, same layout as the source tree. List
  every file you touched and why in an IMPLEMENTATION_REPORT markdown.
- State your assumptions explicitly where the brief leaves latitude.
- New behavior ⇒ new pins. Changed behavior ⇒ updated pins that still
  assert VALUES. If you weaken or delete a test, justify it in the report.
- Suite invariants on our side: `cargo test --workspace` green (4133+ at
  HEAD), zero cold-build warnings, `TONEPOET_REQUIRE_TOOLS=1
  cargo test --test depth_format_matrix` green. Design for that bar.
- If an item can't be done well within this round, SAY SO and ship the
  subset done to standard — a smaller correct delivery beats a complete
  sloppy one.
