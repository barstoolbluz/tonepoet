# Implementation Report: DSF In-Place Tags, Custom-Tag Survival, Completion Authority, P1, Corrective Follow-Ups, and P2

## Verification and delivery scope

The supplied complete source tree was verified before any modification with the required command:

```sh
cd tonepoet && sha256sum -c <(grep -v '^#' docs/handoff_manifest.txt)
```

Result: **564/564 entries passed**. No source file was missing and no scope reduction was required.

This delivery completes:

- P0-1: padded DSF rewrites, append-in-place first tags, crash-safe recovery, collision-free authority names, atomic journal publication, and bounded parallelism for independent targets.
- P0-2: arbitrary source-tag survival and first-class pre-emphasis propagation through single-file and album conversion.
- P0-3: operation authority for Analysis, AccurateRip, CTDB, AR batch, offset correction, and CTDB repair, plus parking-aware `:password` handling.
- P1-1 through P1-7 in the brief.
- Every P2 item in the brief, including the non-UTF-8 authority item previously promoted into P0.
- The first corrective follow-up: the unsupported `PictureType::from(u8)` calls are replaced by an explicit ID3v2 APIC registry mapping with an `Undefined(u8)` fallback and an all-256-values round-trip pin.
- The second corrective follow-up: stored multi-value cardinality is positional rather than row-aggregate, so detail edits warn only for the selected carrier and untouched carriers retain their exact source cardinality.
- The third corrective follow-up: one shared positional collapse detector now covers typed inline/detail edits, detail paste, and `:fix-caps`; paste/capitalization status messages retain the warning instead of overwriting it.
- The fourth corrective follow-up: MusicBrainz population, GNUDB acceptance, MusicBrainz revert/restore controls, split-CUE population, and matching-presentation propagation now return and preserve the same structured positional cardinality-loss report used by manual editor mutations.
- The fifth corrective follow-up: matching-presentation propagation clears source-carrier cardinality when it synthesizes a field absent from the destination, while existing destination rows retain and evaluate their own positional provenance.
- Clean hand-off packaging: the source archive is exported without `.git`, `target`, editor caches, or temporary files, then verified after extraction.

Raw ADTS `.aac` is now an explicit unsupported product mode: it is visible but disabled in the output-container catalog and is rejected before pipeline output planning or metadata rewriting. AAC remains supported in M4A/MP4, where arbitrary metadata is written with the MOV/MP4 metadata-key mode and covered by real-file convergence tests.

No dependency was added. SHA-256 naming and recovery identities use the repository's existing `sha2` dependency; no cryptography was hand-rolled.

## Delivered corrections

### Corrective follow-up: ID3 APIC conversion and archive hygiene

- `src/dsf_tags.rs` no longer relies on a reverse `From<u8>` implementation that `id3` 1.17.0 does not expose.
- APIC codes `0x00..=0x14` map explicitly to their standardized `PictureType` variants; every other byte maps to `PictureType::Undefined(code)`.
- Production artwork replacement/removal and the artwork regression helper use the same conversion function.
- A value-asserting test iterates `u8::MIN..=u8::MAX` and verifies the documented reverse conversion returns the original byte.
- The supplied round-two archive contained 1,526 members, including 896 `.git` members, and was 13,963,507 bytes. The final archive is rebuilt from a clean export and contains no repository metadata or build/cache artifacts.

### Corrective follow-up: per-file multi-value cardinality

The prior report's “P2 cut list: none” statement was premature: its row-wide boolean could not identify which carrier a detail edit would collapse. This report supersedes that claim with positional provenance and detail-path regression coverage.

- `TagEntry` now carries `per_file_stored_value_counts`, aligned with the file-position vectors. The existing aggregate boolean remains only as a compatibility/display summary and is not used to decide which detail slot would lose values.
- Ordinary Lofty reads count every stored item, including duplicate scalar text that is de-duplicated for display.
- DSF snapshots retain exact source-frame counts separately from their ordered, distinct display values; alias canonicalization sums source-frame counts without inventing another media read.
- Single-file, DSF, multi-file, album-with-metadata, and sorted merged-row construction preserve positional counts.
- Track-scoped projections clear file-carrier provenance rather than broadcasting it onto unrelated track positions.
- Detail commit checks only the selected slot and warns only when its saved scalar differs from the original representation. Editing a scalar carrier does not warn; reverting a multi-value carrier to its original representation does not warn.
- Save reduction changes a slot's stored count to zero or one only when that row/slot was actually rewritten. An unrelated save preserves untouched multi-value cardinality.
- Regression pins cover one multi-valued carrier beside one scalar carrier, exact detail-warning behavior, untouched no-write behavior, duplicate DSF-frame counts, album merge propagation, and post-save provenance updates.

### Corrective follow-up: mutation-wide cardinality-loss warnings

The positional model was correct after the second follow-up, but detail paste and `:fix-caps` still mutated scalar projections without consulting it. That left two silent-loss paths. This follow-up closes those bypasses rather than adding unrelated one-off checks.

- `TagEntry::stored_value_collapse_slots` is the shared detector for interactive metadata mutations. It reports a slot only when the carrier originally stored more than one item, the replacement changes the current scalar text, and the replacement is not a revert to the original scalar representation.
- Typed inline edits, typed detail edits, detail paste, and `:fix-caps` all call that detector before mutation.
- Detail paste returns a structured `DetailPasteResult { applied, collapsed_slots }`. The event loop composes both facts into one status, for example `Pasted 2 values; warning: ...`; it no longer replaces the warning with the paste count.
- Album-scoped one-line paste evaluates every replicated target independently, so a `[2, 1, 3]` carrier row reports slots `[0, 2]` rather than treating the row as one aggregate value.
- `:fix-caps` returns structured entry/slot collapse provenance and includes the total affected carriers in its normal `Capitalization applied (...)` status. A no-op capitalization reports no loss.
- Regression pins cover mixed `[2, 1]` multiline paste, scalar-only paste, album-scoped replicated paste with multiple affected carriers, the event-loop status path, capitalization of a multi-valued carrier, and capitalization that makes no textual change.

### Corrective follow-up: provider-driven cardinality-loss warnings

The third follow-up still attached the shared detector only to manual text transformations. MusicBrainz, GNUDB, and MusicBrainz revert/restore controls could replace the same scalar projections without preserving a warning. The earlier claim of complete multi-value-edit protection was therefore still incomplete. This follow-up closes every identified provider/control bypass.

- `MetadataMutationReport` records the number of changed logical fields plus each affected display key and its positional carrier slots. `MetadataMutationReport::between` compares complete pre/post editor snapshots and delegates slot decisions to `TagEntry::stored_value_collapse_slots`; provider code does not carry a second loss predicate.
- MusicBrainz and GNUDB population return structured reports instead of mutating silently. Completion/acceptance status text composes the provider summary and cardinality warning in one message.
- MusicBrainz row toggles, detail toggles, and restore controls return the same report. Row pills, detail pills, `:revert`, `:restore`, context-menu actions, and the generic button dispatcher preserve the warning.
- Split-CUE population merges reports across all presentation tabs. For editor-construction paths populated before the event-loop reducer can retain a pre-mutation snapshot, the reducer reconstructs the report from the durable MusicBrainz proposal/original provenance.
- “Apply MusicBrainz values to matching presentations” returns both the number of changed presentations and the merged mutation report from every sibling tab.
- Headless `tags-mb` emits the same collapse warning to stderr before save.
- Reverting a MusicBrainz-populated field to its exact original scalar representation reports no loss; reapplying or restoring provider values after a manual edit reports the affected carriers.
- Regression pins cover MusicBrainz `[2, 1]` population, GNUDB population, provider completion/acceptance status, Use-MB and restore behavior, no-warning original reversion, row/detail pills, command paths, context-menu paths, split/multi-presentation aggregation, and headless warning wiring.

### Corrective follow-up: matching-presentation provenance isolation

The provider-warning follow-up correctly evaluated existing sibling rows, but its missing-field branch cloned the active presentation's complete `TagEntry`. That copied stored-item counts belonging to different source files into a destination that had no pre-existing carrier for the field, creating a false future collapse warning.

- The missing-field branch now calls `clear_stored_value_provenance()` immediately after cloning the MusicBrainz-populated source row and before installing destination values/originals.
- Both provenance representations are cleared together: `per_file_stored_value_counts` is empty and `has_multiple_stored_values` is false.
- The existing-field branch is deliberately unchanged: it retains the destination row's own `[2, 1]`-style counts, so a real replacement of that destination carrier still reports cardinality loss.
- A regression pin propagates an active `ARTIST` row with `[2, 1]` provenance into a matching sibling that has no `ARTIST` row. It asserts that the created row has no inherited provenance, the propagation report contains no false collapse, and a subsequent scalar edit produces no false warning through the shared detector.
- The pre-existing sibling-row regression continues to assert that an already-present destination `ARTIST` row retains its own `[2, 1]` provenance and reports slot `0` as a real collapse.

### P2 completion

- Startup metadata recovery now resolves database-owned PREPARED transactions before the standalone sidecar scanner can retire a byte-identical `.tonepoet-bak` marker.
- `tonepoet dsf-recover` reports tail journals, resolves PREPARED/COMMITTED tail journals through `recover-tail`, and keeps inspection plus recovery under one per-target lock.
- `restore-backup` reads and validates the marker's leading `DSD ` chunk identifier before any replacement is attempted.
- Non-UTF-8 DSF journal names remain collision-free through native-`OsStr` SHA-256 authority names.
- GNUDB disc-pill navigation commits an in-flight edit to its original page/row before switching pages.
- The byte-identical source-mode setters are merged; reducers use one setter and separately decide whether captured defaults may still apply.
- Probe warnings and source-rate sentinel clamp notices are composed rather than one overwriting the other.
- Mouse regression pins use injected terminal areas instead of host TTY size or the 80x24 fallback.
- Metadata rows carry per-file stored-cardinality provenance; typed edits, detail paste, `:fix-caps`, MusicBrainz/GNUDB population, and MusicBrainz revert/restore controls warn only for carriers that would actually be collapsed, while untouched rows and exact original-value reversions retain their original carrier values and counts without a false warning.

### P0: DSF mutation and authority

- Unavoidable full rewrites seed a persistent 1 MiB ID3 allocation reserve.
- Untagged files append the padded ID3 tail at EOF and patch only the two 8-byte DSF header fields.
- Existing padded tails update in place under the journal.
- Tail journals are versioned and generation-bound; existing v2 replacement-tail journals remain recoverable.
- New journals, rewrite temporaries, and store locks derive collision-free SHA-256 names from the native filename representation:
  - Unix: raw `OsStr` bytes.
  - Windows: little-endian UTF-16 code units.
  - Other targets: an explicitly isolated lossy fallback.
- New journal publication is atomic create-if-absent: a fully fsynced private file is hard-linked into the authority pathname. An existing journal is never replaced.
- Legacy fallback journals remain discoverable. Recovery attributes them by their embedded generation identity instead of reconstructing a target from the fallback filename.
- New hashed journals resolve by their native-filename authority pathname first and then validate the embedded generation identity. This remains unambiguous even when two DSFs are byte-identical.
- Batch planning compares:
  - normalized target pathnames;
  - `same_file` identities, including hard-link aliases;
  - every derived tail-journal authority pathname;
  - every derived store-lock pathname;
  - an existing legacy fallback journal pathname.
- A batch serializes fail-closed if any authority cannot be established or any target/authority aliases another. Otherwise, distinct DSFs may use up to four metadata workers.

### P0: custom tags and pre-emphasis

- Single-file and archive materializers enumerate all source text tags rather than a fixed subset.
- Source-originated text tags carry explicit internal provenance in a namespace disjoint from output-eligible user keys.
- A provenance marker is accepted only when its paired plain source key contains the same value.
- `PRE_EMPHASIS=1` and equivalent affirmative values promote `TrackMetadata.pre_emphasis` during materialization.
- Canonical pipeline fields remain writer-owned; arbitrary source keys are re-emitted under their original keys.
- Native FLAC, Opus, and WavPack writers delete then set emitted keys for rerun convergence.
- FFmpeg metadata rewrites strip inherited metadata and emit the complete authoritative plus preserved payload explicitly.
- M4A/MP4 adds `-movflags +use_metadata_tags`, permitting arbitrary keys such as `MY_NOTE`.
- Raw `.aac` fails closed rather than silently dropping required metadata.
- Tool-gated real-file tests cover single-file and CUE-album conversion to FLAC, Opus, WavPack, MP3, and AAC-in-M4A. They reopen each output after the first and second metadata passes, assert `PRE_EMPHASIS=1` and `MY_NOTE=keep me`, verify container/codec identity, and compare the complete semantic tag map for convergence.

### P0: completion authority and `:password`

- Analysis, AccurateRip, CTDB, AR batch, offset correction, and CTDB repair each receive an independently generated operation ID.
- Dispatch records the exact editor-session guard owned by the operation.
- Reducers reject stale completions before mutating cache, editor, status, or overlay state.
- A completion may enrich only the exact editor session it captured.
- Result overlays publish only when the operation still owns an unobstructed overlay slot.
- Completion handlers no longer clear an unrelated live overlay or strand a parked editor.
- Empty AR/CTDB result sets send terminal completions so operation authority cannot remain active indefinitely.
- `:password` restores its parked metadata editor after submit or cancel.

### P1-1: inline-edit selection contrast

- The centralized inline-edit renderer now uses an inverse pair: `theme.bg` text on a `theme.text_bright` selection surface.
- Both terminal-cursor and embedded-cursor renderers paint selected ranges.
- Convert-screen fields and theme-builder inputs route through the selection-aware renderer, so select-all is visible.
- A palette-wide test asserts at least 4.5:1 selected-text contrast and 3.0:1 selection-surface contrast against the focused field for every built-in theme.

### P1-2: Browse create

- True empty list-space right-click opens the empty-space menu even when another row remains selected.
- Empty-space menus include **New file** and **New folder**.
- `BrowseInlineEditTarget::Create` provides an inline, select-all naming prompt in empty and populated directories.
- Prompt defaults are collision-free; after 10,000 conventional candidates, UUID-backed names avoid a panic or unbounded numeric overflow.
- Direct and prompted creation reject empty names, `.`, `..`, `/`, and `\\`.
- Files use `OpenOptions::create_new`; folders use `create_dir`; neither path overwrites nor creates parent components.
- Archive listings refuse creation.
- Successful creation refreshes the listing, restores the cursor to the new entry, and reprobes the selection.
- `:new-file [name]` and `:new-folder [name]` are parsed, executed, and exposed to command completion.

### P1-3: DSD conversion-log gain lines

- `DSD auto gain margin` is emitted only in Auto mode.
- `DSD manual gain` is emitted only in Manual mode and only when a manual gain exists.
- Tests assert Auto inclusion and Disabled/Manual exclusion behavior.

### P1-4: DSF artwork

- DSF artwork add/remove routes through the DSF ID3 tail writer rather than Lofty's full-file backup/rewrite path.
- Artwork operations use the same bounded lock, journal, cancellation, byte-progress, reserve, and recovery model as ordinary DSF tags.
- Rollback snapshots record the original metadata location and encoded tag.
- For an originally untagged DSF, rollback publishes a generation-bound PREPARED append journal, restores the original 16-byte header patch, truncates the appended allocation without copying audio, fsyncs, and retires the journal.
- Tests assert journal use, absence of a full-file backup, exact byte-for-byte rollback, and exact cancellation restoration.
- Numeric APIC conversion is explicit and exhaustive for the ID3v2 registry, including unknown values; the artwork path no longer depends on an undocumented reverse conversion.

### P1-5: ReplayGain

- Inherited tags are recomputed for lossy output, DSD gain, sample-rate conversion, unknown source-relative rate, bit-depth reduction, and float-to-integer conversion.
- `prevent_clipping` now controls loudgain's `-k` argument.
- Track-only scans remove stale album gain/peak tags after loudgain completes.
- The durable log records whether ReplayGain was disabled, had no successful outputs, trusted complete inherited tags, recomputed and why, or failed under a stated policy.
- Tests pin the new predicates, `-k` argument behavior, stale album-tag removal, and log wording.

### P1-6: durable tolerant-read warnings

- `PreparedTrack` carries serde-compatible materializer warnings.
- Single-file/archive materializers preserve accepted metadata/container degradation warnings.
- Conversion-log per-track details render each warning durably.
- Existing constructors were updated explicitly with empty warning vectors where no warning source exists.

### P1-7: secrets

- Config loading degrades on every pending-publication reconciliation error instead of bricking startup; strict save-time reconciliation remains unchanged.
- The warning names both config and journal paths and gives a concrete repair/headless-retirement remedy.
- `tonepoet config --retire-secret-journal` runs before ordinary config loading, acquires the publication target's store lock, parses and validates the journal, removes it durably without contacting the secret backend, and reports the number of references that may remain orphaned.
- A malformed journal remains intact and produces an explicit error.
- `clear_queue` retires queue-owned archive-password references.
- MRU keychain loading degrades per unresolved reference: valid entries remain usable and skipped references are shown as a visible warning.
- Browse/automation admission's non-deduplicating behavior is documented as intentional because callers may enqueue the same source with distinct resolved settings/destinations.

## Files touched

### Integrity and report

- `IMPLEMENTATION_REPORT_DSF_INPLACE_TAGS_OVERLAYS_UX.md`
- `docs/handoff_delta_manifest_dsf_inplace_tags_overlays_ux_round2.txt`

### DSF, configuration, secrets, and CLI

- `src/config.rs` — native-filename SHA-256 store-lock authorities; tolerant load reconciliation.
- `src/dsf_tags.rs` — padded/append paths, journal naming/publication/recovery, artwork operations, explicit APIC mapping, exact DSF source-frame cardinality, tail-journal CLI APIs, DSF backup sniffing, and pins.
- `src/main.rs` — headless secret-journal retirement, DSF tail-journal status/recovery CLI and parser pins, and headless MusicBrainz cardinality-loss reporting.
- `src/secret_store.rs` — validated, locked, durable headless retirement.

### Conversion pipeline and materializers

- `src/convert/formats.rs` — disable raw AAC and state its metadata limitation.
- `src/convert/mod.rs` — queue-secret retirement and admission-policy documentation.
- `src/convert/pipeline/types.rs` — provenance helpers and durable warnings.
- `src/convert/pipeline/materializer_single.rs` — all-text source tags, pre-emphasis, warnings.
- `src/convert/pipeline/materializer_archive.rs` — provenance/pre-emphasis/warnings.
- `src/convert/pipeline/materializer_bluray.rs`
- `src/convert/pipeline/materializer_cue.rs`
- `src/convert/pipeline/materializer_dvda.rs`
- `src/convert/pipeline/materializer_dvdv.rs`
- `src/convert/pipeline/materializer_sacd.rs`
  - The five files above explicitly initialize the new warning field where no tolerant-read warning source exists.
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/source_heuristics.rs`
- `src/convert/pipeline/track_executor.rs`
- `src/convert/processor.rs`
  - The four files above propagate or explicitly initialize the new `PreparedTrack.warnings` field.
- `src/convert/pipeline/stages.rs` — metadata policy/writers, raw-AAC refusal, real-file matrix, ReplayGain, DSD log gating, durable warnings.

### TUI authority, Browse, editing, artwork, secrets UI, and P2

- `src/tui/app.rs` — operation/keychain state, Browse create target, duplicate source-mode setter removal, save-time per-slot cardinality reduction, and structured MusicBrainz propagation results for matching presentation tabs.
- `src/tui/command.rs` — operation dispatch, Browse create commands, stored-cardinality initialization, composed `:fix-caps` loss status, and warning-preserving `:revert`/`:restore` paths.
- `src/tui/context_menu.rs` — Browse create actions, stored-cardinality initialization, and warning-preserving MusicBrainz row/detail controls.
- `src/tui/draw.rs` — visible per-reference keychain warnings.
- `src/tui/draw_browse.rs` — inline create row and cursor.
- `src/tui/draw_metadata.rs` — selection-aware inline renderer integration.
- `src/tui/draw_output_options.rs` — selection-aware inline renderer integration.
- `src/tui/draw_overlays.rs` — stored-cardinality initialization in overlay fixtures/models.
- `src/tui/event_loop.rs` — completion-family authority reducers, DB-first startup recovery, composed probe/clamp status, merged source-mode setter use, composed detail-paste loss status, structured MusicBrainz completion/split-CUE reporting, and pins.
- `src/tui/gnudb.rs` — stored-cardinality initialization, file-provenance retirement for track-scoped projections, and structured provider-population reports.
- `src/tui/inline_edit.rs` — structural selection contrast and palette tests.
- `src/tui/keybindings.rs` — completion dispatch, parking, Browse creation, context hit behavior, GNUDB disc-pill commit/acceptance reporting, hermetic mouse seams, shared typed/paste/`:fix-caps` collapse detection, MusicBrainz row/detail pill reporting, split-CUE report aggregation, matching-presentation status composition, track-provenance retirement, and tests.
- `src/tui/keychain.rs` — per-reference tolerant loading.
- `src/tui/message.rs` — completion operation IDs.
- `src/tui/metadata_view_models.rs` — stored-cardinality initialization.
- `src/tui/musicbrainz.rs` — stored-cardinality initialization, file-provenance retirement for track-scoped projections, and structured provider-population reports.
- `src/tui/preemphasis/metadata.rs` — shared affirmative parser.
- `src/tui/probe.rs` — DSF worker authority policy, DSF artwork routing/rollback, exact per-slot cardinality capture/merge/alignment, the shared scalar-replacement loss detector, structured provider/control mutation reports, and pins.
- `src/tui/theme_builder.rs` — selection-aware theme-builder inputs.

## Assumptions and NEEDS-VERIFICATION seams

- The existing `sha2` SHA-256 API and `same-file` identities are authoritative because both were already repository dependencies and patterns.
- `std::fs::hard_link` is used as same-directory atomic create-if-absent journal publication. Unsupported filesystems fail closed; there is no unsafe check-then-rename fallback.
- Advisory file locks remain bounded at the pre-existing two-second acquisition limit. Correct planning prevents independent DSFs from contending on one authority; genuine same-target contention remains a visible failure rather than an unbounded UI stall.
- `Cargo.lock` pins `id3` 1.17.0. The APIC byte-to-enum direction is implemented locally from the ID3v2 registry; the test uses the crate's documented enum-to-byte conversion. The wider artwork API still requires compilation on the acceptance host because this environment has no Rust toolchain.
- External metadata-tool behavior is verified by tool-gated real-file tests when the required tools are installed. The no-tool default keeps ordinary workspace tests hermetic; `TONEPOET_REQUIRE_TOOLS=1` turns missing matrix tools into failures.
- Raw ADTS AAC cannot provide the required arbitrary metadata contract and is therefore disabled/rejected rather than silently lossy.

## Value-asserting pins

Notable added or strengthened tests assert values rather than mere presence:

- Compact DSF growth rewrites once with at least 1 MiB reserve; the next growth preserves file size and audio prefix.
- Untagged first tagging appends at the old EOF and preserves every original byte outside offsets 12..28.
- PREPARED/COMMITTED append recovery, cancellation rollback, v2 compatibility, and exact artwork rollback.
- Two distinct non-UTF-8 DSFs derive different journals and locks.
- A non-UTF-8 DSF and literal `audio.dsf` derive different journals and locks.
- Concurrent publication never replaces an existing journal.
- The scheduler parallelizes independent non-UTF-8 authorities and serializes shared legacy authority/hard-link aliases.
- Legacy fallback recovery selects the correct non-UTF-8 target by embedded identity.
- Single-file and album real outputs retain both required tags through two passes for FLAC, Opus, WavPack, MP3, and M4A/AAC.
- Raw `.aac` planning and metadata writing fail closed.
- Every completion family preserves the exact dirty editor/session it captured.
- All built-in palettes clear the selection contrast thresholds.
- Browse create/refresh/cursor, overwrite refusal, invalid components, archive refusal, command parsing/completion, and empty-space menu behavior.
- DSD log gain-line gating.
- ReplayGain recompute predicates, clipping flag, stale album-tag removal, and provenance log labels.
- Durable per-track warnings.
- Config-load degradation, valid/malformed headless journal retirement, queue-secret retirement, and per-entry keychain degradation.
- All 256 APIC picture-type bytes round-trip through the explicit mapping.
- DB PREPARED startup recovery consumes a byte-identical DSF marker before standalone scanning.
- Tail-journal inspection/recovery asserts state, operation, original size, committed size, exact rollback, and committed-generation retention.
- Legacy restore refuses a non-DSF marker without changing either generation.
- GNUDB disc-pill navigation commits the edit to the old page before activating the new page.
- Probe warning text and the sentinel-clamp notice both survive one reducer completion.
- Multi-value carrier detection preserves de-duplicated display text while recording exact cardinality; a mixed `[multi, scalar]` row warns only when the multi-valued slot changes through typing, paste, or capitalization, does not warn for the scalar slot, a revert, or a no-op transformation, and remains no-write when untouched.
- Detail-paste pins assert positional `[2, 1]` loss, scalar-only non-loss, album-scoped replication across `[2, 1, 3]`, and preservation of the composed warning in the event-loop status path.
- `:fix-caps` pins assert structured carrier/slot provenance when text changes and no warning when capitalization is a textual no-op.
- MusicBrainz and GNUDB provider pins assert `[2, 1]` positional behavior: only the multi-valued carrier is reported while the scalar carrier is not falsely warned.
- MusicBrainz control pins assert that Use-MB and restore report a collapse, exact reversion to original values does not, and row-pill, detail-pill, command, and context-menu status paths preserve the warning.
- Completion pins exercise the actual MusicBrainz apply reducer and guarded GNUDB review acceptance; both retain their normal provider summary and the structured warning. Matching-presentation and split-CUE report aggregation are value-pinned separately.
- DSF duplicate frames remain de-duplicated in display text while merged ordinary and album readers retain exact per-file counts; save reduction changes only rewritten slots from multi-value to scalar provenance.
- Mouse tests derive coordinates from injected 100x40 areas rather than an ambient terminal.

## Worst-case I/O accounting

Definitions:

- `B`: bytes before the DSF metadata offset.
- `E`: newly encoded ID3 bytes.
- `R = 1,048,576`: seeded reserve.
- `P = E + R`: padded new allocation.
- `Q`: existing in-place metadata allocation.
- `S = min(identity domain, 65,536)`; first and last samples are read separately, so bounded identity reads are at most `2S` even when ranges overlap.
- `H = 74`: v3 tail-journal header bytes.
- `F`: external-tool input bytes.
- `O`: resulting external-tool output bytes.
- `J`: pending secret-journal bytes.
- `L`: final durable conversion-log bytes.

Counts below identify explicit Tonepoet fsync calls. External tools own their internal durability policy.

### DSF tag and artwork paths

**Compact tagged growth before this work**

- Reads `B`; writes `B + E` plus a 16-byte temporary-header overwrite.
- Bytes moved: `2B + E + 16`.
- Fsyncs: 2.
- Because no reserve remained, later growth repeated the same full-prefix pass.

**One unavoidable seeded rewrite**

- Reads `B`; writes `B + P` plus a 16-byte temporary-header overwrite.
- Bytes moved: `2B + P + 16`.
- Audio bytes copied: `B`.
- Fsyncs: 2 — temporary file, parent after publication.
- Increment over the former rewrite: exactly `R` written bytes, once.

**Existing padded-tail update**

- Reads `2S + Q`.
- Writes `H + Q` journal bytes, `Q` target bytes, and one COMMITTED byte.
- Bytes moved: `2S + 3Q + 75`.
- Audio bytes copied: 0.
- Fsyncs: 5 — journal temp, parent publication, target, journal state, parent removal.

**Untagged first-tag append**

- Reads `2S + 16`.
- Writes `H + 16` journal bytes, `P` appended bytes, 16 target-header bytes, and one COMMITTED byte.
- Bytes moved: `2S + P + 123`.
- Audio bytes copied: 0.
- Fsyncs: 6 — journal temp, parent publication, append, header patch, journal state, parent removal.

**PREPARED replacement-tail recovery**

- Reads `H + 2S + Q`; writes `Q` restored bytes.
- Bytes moved: `2S + 2Q + 74`.
- Fsyncs: 2 — target and parent removal.

**PREPARED append recovery**

- Reads `H + 2S + 16`; writes 16 restored header bytes; truncates without copying up to `P` appended bytes.
- Bytes moved: `2S + 106`.
- Fsyncs: 2.

**Exact rollback of artwork added to an originally untagged DSF**

- Journal publication writes `H + 16 = 90` bytes.
- Recovery reads `H + 16 + 2S` and writes 16 target-header bytes; truncation copies 0 bytes.
- Aggregate bytes moved: `2S + 196`, plus the existing bounded DSF geometry/header inspection reads.
- Fsyncs: 4 — rollback-journal temp, parent publication, restored/truncated target, parent removal.

**COMMITTED recovery**

- Replacement tail: reads `H + 2S`, writes 0 target bytes, one parent fsync.
- Append: reads `H + 2S` plus bounded strict DSF geometry/tag-boundary inspection, writes 0 target bytes, one parent fsync.

**Atomic journal publication**

- The hard-link publication itself moves 0 payload bytes.
- The private journal has already been written and fsynced; one parent-directory fsync makes the authority pathname durable.
- If private-name removal fails, both names identify the same inode; recovery cleanup handles the extra name.

**DSF authority planning**

- File-content bytes read/written: 0.
- Fsyncs: 0.
- One identity handle and a bounded set of pathname comparisons per DSF target.

### Corrective and P2 paths

**APIC byte mapping**

- Runtime file bytes read/written: 0 beyond the existing artwork tag read/write path.
- Fsyncs added: 0.
- The change is a pure byte-to-enum conversion.

**Startup recovery ordering**

- Bytes read/written and fsync counts are unchanged from the two existing recovery mechanisms; only their order changes.
- The DB transaction consumes its authoritative marker first, then the directory scanner performs its ordinary bounded scan.

**`dsf-recover status`**

- Tail-only status reads the `H`-byte journal header and up to `2S` target identity bytes; a COMMITTED append may additionally perform bounded DSF geometry/tag-boundary reads.
- Legacy-marker status reads metadata only when lengths differ; when lengths match at `D` bytes each, worst-case comparison reads `2D` bytes.
- Target/marker bytes written: 0. Fsyncs: 0, apart from first-ever creation of the pre-existing persistent lock authority.

**`dsf-recover recover-tail`**

- Inspection and recovery execute under one target lock. The inspection adds `H + 2S` reads, plus bounded append geometry checks when applicable, before the recovery counts already listed above.
- PREPARED recovery then writes only the journaled original tail (`Q`) or the 16-byte DSF header patch and truncates the append; COMMITTED recovery writes 0 target bytes.
- Fsyncs remain 2 for PREPARED recovery (target plus parent after journal removal) and 1 for COMMITTED retirement (parent after journal removal).

**Legacy `restore-backup` validation and publication**

- The new validation itself reads exactly 4 marker bytes and writes 0 bytes.
- Including the pre-existing explicit inspection and atomic restore, equal-size worst case for a `D`-byte marker is `4D + 4` bytes moved: `2D` comparison reads, 4 header bytes, `D` restore-source reads, and `D` temporary writes.
- Publication fsyncs: 3 — restore temporary, parent after replacement, parent after marker removal.
- A non-DSF marker stops after the comparison plus 4-byte sniff and performs 0 writes/fsyncs.

**GNUDB navigation, source-mode merge, status composition, mouse-test seams, and multi-value warnings**

- Persistent file bytes read/written: 0.
- Fsyncs added: 0.
- Multi-value provenance adds one `usize` per positional row slot, one aggregate compatibility boolean per row, and one cardinality entry per canonical DSF key. It reuses already parsed Lofty/ID3 objects and does not add another media-file pass.
- The shared collapse detector scans only the mutation's in-memory target slots. Detail paste is `O(T log T)` for at most the row dimension because reported slot indices are sorted/de-duplicated; `:fix-caps` is `O(V)` plus that bounded per-entry slot scan. Persistent I/O remains 0 bytes and 0 fsyncs.

**MusicBrainz/GNUDB population and MusicBrainz controls**

- Persistent file bytes read/written: 0.
- Fsyncs added: 0.
- Provider population snapshots the already resident metadata rows and compares them after mutation. Worst-case work is `O(E_before × E_after + S)` with the current key lookup, where `E` is editor-row count and `S` is the total number of positional replacement slots examined; memory is one cloned in-memory row set plus the bounded structured report.
- Row/detail toggle and restore paths clone one in-memory `TagEntry` and scan only that row's slots. Split-CUE and matching-presentation paths merge the per-tab reports without another media read.
- Clearing cloned provenance for a destination-missing field mutates two in-memory fields only; it reads/writes 0 persistent bytes and adds 0 fsyncs.
- Headless `tags-mb` adds only stderr text when loss is detected; it does not add a source read, output write, or fsync.

### Source tags and external writers

**Source-tag enumeration/provenance**

- Additional file bytes read: 0; tags are enumerated from the already parsed tag object.
- Additional file bytes written: 0.
- Fsyncs: 0.
- Memory: one plain value plus one provenance value per retained normalized source key.

**FLAC/Opus/WavPack native tag tools**

- Tonepoet adds 0 file-copy bytes and 0 explicit fsyncs.
- Conservative external-tool worst case: `F` bytes read and `O` bytes written per file.
- Delete-then-set convergence changes arguments, not the number of Tonepoet passes.

**MP3/M4A/MP4 FFmpeg metadata rewrite**

- FFmpeg reads `F` and writes `O` to a same-directory temporary using stream copy.
- Tonepoet copies 0 bytes during publication.
- Explicit fsyncs: 2 — temporary file, parent after rename.
- Metadata size increases `O` only by the muxer's representation of the preserved payload.

**Raw AAC refusal**

- Rejected before output materialization/metadata writing.
- File bytes read/written: 0.
- Fsyncs: 0.

### ReplayGain

**loudgain scan**

- Worst case: reads the complete successful output set `ΣF_i`; tag-write bytes and internal fsync behavior are tool-owned.
- `prevent_clipping` changes one argument and no Tonepoet I/O.

**Track-only stale album-tag cleanup**

- Conservative Lofty worst case per successful output: read `F_i`, rewrite `O_i`.
- Aggregate worst case: `ΣF_i + ΣO_i` bytes moved.
- Tonepoet adds 0 explicit fsync calls; Lofty owns persistence behavior.

### Browse, logs, and secrets

**Browse file/folder create**

- New empty file payload: 0 bytes; folder payload: 0 bytes.
- Directory refresh reads directory entries and existing classification metadata according to the pre-existing Browse scan path.
- Explicit fsyncs: 0. Creation has ordinary OS/filesystem durability semantics; this feature does not claim crash-durable directory publication.

**Durable warning/ReplayGain log additions**

- No additional source/audio reads.
- The final log grows only by the encoded warning/provenance lines; total published log bytes remain `L` under the existing atomic log path.
- No new fsync beyond that existing log-publication path.

**Headless secret-journal retirement**

- Reads `J`; writes 0 journal bytes; removes the journal.
- Fsyncs: 1 parent-directory fsync for durable removal.
- First-ever creation of the persistent store-lock marker may additionally write the small marker, fsync the lock file, and fsync its parent; subsequent acquisitions write 0 bytes.
- Secret-backend I/O: 0 by design.

**Queue/keychain secret retirement/resolution**

- Queue clear adds no queue-file pass; each backend deletion is implementation-owned by the configured secret backend.
- MRU loading performs the same reference-file read as before and one backend lookup per reference. Skipping an unavailable reference adds 0 file writes and 0 fsyncs.

**Selection rendering, operation authority, and DSD log gating**

- Persistent file bytes read/written: 0 beyond the already existing log publication.
- Fsyncs added: 0.

## P2 completion and cut list

After the provider/control correction above, all eight P2 items in the brief are implemented, including warning coverage for every identified scalar-replacement mutation path. **P2 cut list: none.** The prior completion claim was not sufficient while provider mutations bypassed the detector; this report supersedes it.

Explicitly out of scope and unchanged:

- Companion-CUE behavior.
- DFF tag writing.
- Ambiguous-EAW terminal handling.

## Validation performed and acceptance limitations

Performed in this environment:

- Original archive manifest verification before modification: 564/564 entries passed.
- Supplied archive inventory: 1,526 members, 896 under `.git`, 13,963,507 compressed bytes.
- `git diff --no-index --check` against the prior corrected-P2 export; no whitespace errors were reported.
- Search for every removed `PictureType::from(code)` and `set_source_mode_preserving_format_selection` call: none remain in source/tests.
- Exhaustive scan of all 79 `TagEntry` construction sites for exactly one positional stored-cardinality field, plus all `DsfTagSnapshot` literals after adding exact source-frame counts.
- Static review of every file-to-track scope transition and every save-time provenance update, including partial/untouched-slot behavior.
- Shared-detector call-site review for typed inline commit, typed detail commit, detail paste, and `:fix-caps`; direct event-loop paste status is pinned so the warning cannot be overwritten by `Pasted N values`.
- Value pins for `[2, 1]` multiline paste, scalar-only paste, `[2, 1, 3]` album replication, multi-value capitalization, and capitalization with no textual change.
- Provider/control call-site review for every production invocation of MusicBrainz population, GNUDB population, MusicBrainz row/detail toggles, MusicBrainz restore, matching-presentation propagation, and split-CUE population.
- Value pins for MusicBrainz and GNUDB `[2, 1]` population, actual MusicBrainz completion and GNUDB acceptance statuses, Use-MB/revert/restore semantics, row/detail pills, `:revert`, `:restore`, context menus, matching presentations, split-CUE aggregation, and headless warning wiring.
- Matching-presentation provenance-isolation pin: a newly created sibling field clears cloned `[2, 1]` source provenance and produces no false warning on a later scalar edit, while the existing-row `[2, 1]` loss pin remains intact.
- Changed-signature/call-site review for DSF tail recovery, source-mode installation, mouse helpers, GNUDB page switching, and metadata commit.
- Merge-marker, archive-content, and whitespace scans.
- Final delta-manifest verification and baseline/delta coverage verification after clean export.
- Fresh extraction and byte-for-byte comparison against the clean staging tree.
- Final archive inventory: `630` members with zero `.git`, `target`, Python cache, editor-swap, or `.DS_Store` members. The exact compressed byte count is reported alongside the delivered artifact rather than embedded here, because embedding it would change that count.

The four required Rust acceptance commands were executed, but the environment has no `cargo` binary. Each command exited 127 with `cargo: command not found`:

```text
cargo fmt --check
RUSTFLAGS="-D warnings" cargo check --workspace --all-targets
cargo test --workspace
TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix
```

Consequently, formatting, compilation, warning freedom, the 4,371-test workspace result, and the tool-gated depth matrix are **not certified here**. They remain mandatory acceptance-host gates. No report statement treats static inspection as a substitute.
