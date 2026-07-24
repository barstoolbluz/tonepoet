# Implementation report: native multi-FILE CUE album metadata editing

Date: 2026-07-24

## Executive summary

This corrective delivery completes the narrow front-end and metadata-precedence round in `brief_multifile_cue_album_and_metadata_edit.md` without modifying `src/convert/pipeline/**` or redesigning conversion, metadata transactions, recovery, or the editor UI.

It implements four behaviors:

1. One admitted sidecar CUE referencing two or more audio images opens in the existing metadata editor as one continuous album, with every row retaining its owning image through `track_audio_paths`.
2. MusicBrainz and GNUDB geometry preserves authored CUE `INDEX 01` boundaries directly in the native 75-frames-per-second domain. Only the final track in each `FILE`, whose end is physical EOF rather than another authored index, converts sample count to sectors under an explicit floor-to-complete-sector policy.
3. Cue-source authority is selected through `CueSidecarPolicy` before the editor model is built. The retained identity is typed as sidecar or embedded; save-time sidecar write-back occurs only when policy selected the sidecar.
4. Rejected CUEs discovered beside selected audio are fatal only when they are relevant to that selection. An unrelated malformed or incomplete CUE cannot veto ordinary MusicBrainz or GNUDB lookup.

The Foxy album-edit correction now covers the actual ordinary single-image editor route. Folder, CUE-file, and image entry preserve the admitted surface through the generic editor path, resolve authority through the same `CueSidecarPolicy` helper used by the unified path, and store the selected typed identity on the production `MetadataEditorState`. Album metadata is regenerated into the embedded CUESHEET and, when the selected source is a sidecar, written to that exact sidecar only after every affected image save succeeds.

## Scope discipline

No file under `src/convert/pipeline/**` changed. No conversion-action, database, crash-recovery, metadata-journal, folder-sanitization, duplicate-format-copy selection, cue-less-folder heuristic, configuration setting, or new editor mode was added.

The implementation reuses:

- `admit_split_cue_member` for canonical sidecar membership and synthetic-album admission;
- the existing `CueSidecarPolicy` variants and precedence semantics;
- `MetadataCueSurface.audio_paths` and `track_audio_paths` for ownership;
- the existing unified CUE-album editor and regenerated synthetic-sheet path;
- the existing MusicBrainz cache and rate-limited text/TOC lookup paths;
- the existing sidecar byte-span writer and atomic replacement path.

## Deliverable 1: one metadata-editor album

`open_metadata_editor_impl` treats one admitted CUE surface with multiple distinct member images as CUE-shaped, just as it already treats a merged split-CUE group. Folder, CUE-file, and member-image entry all route to `open_metadata_editor_for_cue_surfaces_with_active` rather than falling through to image-level rows.

The existing unified surface remains authoritative:

- tracks are positional and continuous;
- `CueAlbumTrackSource.audio_path` maps every row to its owning image;
- the policy-selected source is retained in `PresentationTab.cue_source` as `MetadataCueSource::Sidecar` or `MetadataCueSource::Embedded`;
- no tab, widget, or editor mode was added.

A regression opens the same four-track/two-image fixture from its folder, its `.cue`, and one member image and asserts one album surface, continuous titles, exact row-to-image ownership, and the selected source identity.

## Deliverable 2: exact MusicBrainz and GNUDB geometry

### Admission and probing

`MultiFileCueLayout` is a TUI-facing view of an already-admitted member. It contains the parsed sheet, distinct member images, and track-position-aligned image paths. Native multi-FILE detection requires at least two resolved member images and the existing synthetic-album role.

`probe_multi_file_cue`:

- probes each canonical member path once;
- rejects zero sample rate, zero samples, missing `INDEX 01`, inconsistent mappings, non-increasing boundaries, boundaries beyond physical EOF, and more than 99 tracks;
- preserves each authored start and next-track boundary as CUE frames, without converting through samples;
- marks only the final track in each `FILE` as EOF-ended;
- compares CUE frame positions to physical EOF exactly with checked `u128` rational arithmetic;
- never accumulates CUE timestamps across `FILE` boundaries.

### Continuous TOC

`multi_file_cue_info_to_cd_sectors` starts at the standard 150-sector lead-in and concatenates track durations:

- authored `INDEX 01` to `INDEX 01` duration is exactly `end_frame - start_frame`;
- a `FILE`-ending track computes `floor(total_samples * 75 / sample_rate) - start_frame`;
- the EOF rule deliberately counts only complete CD sectors represented by the physical file;
- all multiplication, subtraction, accumulation, and narrowing are checked;
- zero-sector tracks and invalid lead-out geometry fail closed.

This eliminates the prior double-truncation defect at rates not divisible by 75. The 32 kHz regression proves that a one-frame authored duration remains one sector and a 100-frame duration remains 100 sectors. It also pins the resulting GNUDB disc ID.

For MusicBrainz:

- exact geometry launches the existing TOC lookup;
- any probe or geometry failure launches the existing album text-search fallback when album, artist, catalog, or year metadata exists;
- otherwise the status line explains why neither lookup is possible;
- a recognized native multi-FILE CUE never falls through to naive whole-file concatenation.

For GNUDB:

- the same exact sector vector feeds the integer-only disc-ID bridge;
- invalid geometry fails closed with a status-line error;
- multiple native multi-FILE album copies require explicit selection rather than implicit deduplication.

### Alien rejected-CUE isolation

Discovery still scans visible CUEs in selected folders and audio-file parents, but an admission error is retained only when the rejected CUE:

- was selected explicitly;
- is inside a folder selected as the lookup source; or
- resolves at least one `FILE` reference to selected audio.

A rejected CUE that cannot be associated with an ordinary selected file is ignored for that file-level operation. A rejected CUE that references selected audio remains a fail-closed error. This preserves existing single-image and ordinary-audio lookup behavior without allowing a relevant cue-shaped album to fall through to naive concatenation.

## Deliverable 3: saved album edits reach conversion

Cue source identity is retained from policy resolution through save. This includes both editor construction routes: the unified multi-surface/native-multi-FILE builder and the ordinary generic single-image builder used by the Foxy shape. The generic route no longer discards its admitted `MetadataCueSurface`; after tag loading it matches that surface to the expanded audio image, applies the shared policy-selected source resolver, and assigns the resulting `MetadataCueSource` to the actual editor state. No directory rediscovery is introduced. The sidecar write-back plan requires `MetadataCueSource::Sidecar`; `MetadataCueSource::Embedded` cannot schedule a neighboring sidecar mutation.

Before sidecar write-back:

- a single-image CUE is re-admitted and must still resolve exactly one member matching the open image;
- a native multi-FILE CUE is re-admitted under `SidecarOnly`, because the retained typed identity has already established sidecar authority, and its complete track-to-image mapping must still equal the editor snapshot;
- all member-image CUESHEET values must agree;
- all affected image saves must succeed before the sidecar advances.

A failed revalidation does not guess, silently skip, or rewrite another CUE. It returns the existing sidecar-stale result with a specific reason.

The sidecar writer treats album `CATALOG` as editable metadata alongside album `TITLE`, `PERFORMER`, `SONGWRITER`, date/year, and genre. Its byte-targeted path preserves original keyword casing, indentation, separator whitespace, trailing whitespace, source encoding, line endings, comments, unknown lines, `FILE`, `TRACK`, `INDEX`, `PREGAP`, `FLAGS`, and `ISRC`. An identical second save is a byte-preserving no-op.

The Foxy-shaped routing regressions open the same sidecar-plus-embedded single-image fixture through the actual folder, CUE-file, and image production entry paths. The deterministic 44.1 kHz / 4,410-sample FLAC uses valid but structurally distinct track-2 offsets—`00:00:03` in the sidecar and `00:00:05` in the embedded sheet—so both layouts remain before physical EOF while still proving policy selection. Under the default sidecar policy, each resulting editor state must retain `MetadataCueSource::Sidecar`, edits to `ALBUM` and `CATALOGNUMBER` must produce a real write-back plan, and the selected sidecar must contain the values conversion reads. Repeating all three routes under `PreferEmbedded` must retain `MetadataCueSource::Embedded`, regenerate the embedded sheet, produce no sidecar plan, and leave the sidecar byte-identical. The fixture uses the checked-in valid FLAC test asset, so these tests do not silently skip when FFmpeg is absent.

## Deliverable 4: policy-driven cue-source seam

No preference setting or UI was added.

The front-end now has one explicit no-config default, `DEFAULT_FRONTEND_CUE_POLICY = PreferSidecar`, and policy-aware entry points accept a `CueSidecarPolicy` value. Source resolution mirrors the existing pipeline precedence:

- `SidecarOnly`: require the admitted sidecar and retain its identity; when a valid embedded sheet has the same track count and every `INDEX 01` matches, mirror the pipeline freshness upgrade for rendered metadata while keeping the sidecar as the write-back target;
- `PreferSidecar`: apply the same sidecar identity and structurally-matched embedded metadata upgrade; mismatched or invalid embedded metadata keeps the sidecar, while non-sidecar entry paths retain their existing embedded fallback when no matching sidecar is admitted;
- `PreferEmbedded`: use a valid embedded CUESHEET when present, otherwise fall back to the sidecar;
- `EmbeddedOnly`: require a valid embedded CUESHEET;
- `IgnoreCue`: do not admit a CUE-shaped source.

The editor model stores the result as a typed `MetadataCueSource`. Rendering uses the selected sheet and cue text. Both the unified CUE-surface route and the generic single-image route receive the policy explicitly and retain the same selected identity, so an embedded-selected editor never writes the sidecar and a sidecar-selected Foxy editor cannot silently lose write-back authority. Native multi-FILE sidecar admission also accepts an explicit policy; because embedded CUESHEETs are single-image by definition in the existing pipeline, `EmbeddedOnly` cannot resolve a native multi-FILE sidecar album and fails explicitly.

A future folder-level preference needs only to supply a different policy value at the existing entry points. It does not require changing editor rendering, source identity, save routing, TOC geometry, or conversion precedence.

## Files changed

The cumulative implementation relative to the supplied original baseline changes:

- `src/tui/app.rs`
- `src/tui/cue_parser.rs`
- `src/tui/command.rs`
- `src/tui/context_menu.rs`
- `src/tui/gnudb.rs`
- `src/tui/keybindings.rs`
- `src/convert/cue_parser.rs`
- `docs/cue_sidecar_writeback_policy.md`
- `IMPLEMENTATION_REPORT_MULTIFILE_CUE_ALBUM_METADATA_EDIT.md`
- `docs/handoff_manifest.txt` (regenerated after all changes)

The corrective-v4 delta relative to corrective v3 changes only `src/tui/keybindings.rs`, this report, and the regenerated manifest. It corrects the deterministic Foxy fixture geometry without changing production behavior.

## Regression coverage added

- two-`FILE` CUE with differing per-file track counts and hand-computed local-index TOC;
- three-`FILE` CUE, including a 32 kHz member, with exact local-index geometry;
- four-`FILE` CUE with differing sample rates and continuous TOC;
- 32 kHz one-frame and 100-frame authored durations, proving no sample round trip;
- exact GNUDB disc-ID result for the 32 kHz fixture;
- mixed FLAC/WavPack admission and probe-fact geometry without an optional-tool skip;
- missing-member admission failure and over-99-track failure;
- unrelated rejected CUE does not veto selected ordinary audio;
- rejected CUE referencing selected audio still fails closed;
- real multi-FILE probe failure dispatches the cached MusicBrainz album-text fallback;
- MusicBrainz TOC acceptance of generated geometry;
- GNUDB exact-sector equivalence and invalid-geometry rejection;
- folder/CUE/member-image editor routing to one unified album;
- sidecar/embedded policy selection, structurally-matched metadata upgrade, mismatch fallback, required-source, non-multi-FILE classification, and save-routing behavior;
- actual folder/CUE-file/image Foxy routing under the default sidecar policy, including real save planning and write-back for `ALBUM` and `CATALOGNUMBER`;
- the same three Foxy routes under `PreferEmbedded`, proving selected-source identity changes consistently and sidecar bytes remain unchanged;
- deterministic checked-in FLAC coverage for those routing tests, with no optional-tool skip;
- native multi-FILE sidecar write-back gated on every member-image save;
- concurrent sidecar membership drift reported instead of rewritten;
- CATALOG byte-format preservation, UTF-8 BOM insertion, parsing, and no-op second save.

## Validation performed in this sandbox

- Verified all 713 entries in the supplied SHA-256 manifest before editing.
- Verified the bundled brief is byte-identical to the separately uploaded brief.
- Compared the final tree directly with the supplied baseline tree and produced an exact patch.
- Confirmed no diff under `src/convert/pipeline/**`.
- Ran whitespace/error-marker checks over the complete diff.
- Ran Rust-aware lexical delimiter, string, character, raw-string, and comment checks over every changed Rust file.
- Independently recomputed the 2-file, 3-file, 4-file, 32 kHz, MusicBrainz, and GNUDB expectations.
- Probed the deterministic Foxy FLAC as 44.1 kHz / 4,410 samples and verified the sidecar and embedded track-2 starts resolve to 1,764 and 2,940 samples, both strictly before EOF.
- Regenerated and verified the complete handoff manifest and delivery archives after packaging.

The sandbox contains no Rust, Cargo, rustfmt, or Nix toolchain. Network name resolution is also unavailable, so a toolchain could not be installed. Consequently this environment cannot run `cargo fmt`, type-check, compile, execute tests, or inspect cold-build warnings. The applier must run:

```bash
cargo fmt --all -- --check
cargo test --workspace --no-fail-fast
```

The full suite must report zero failures and zero new cold-build warnings before hand-off acceptance. This report does not claim that compiler gate has run.

## Behavioral decisions and precedence rules

1. One admitted CUE with two or more distinct member images and synthetic-album semantics is one album. Multiple such CUEs are not deduplicated; the user must select one copy.
2. Authored track boundaries remain exact CUE frames. Only a final track ending at physical EOF is rounded, by flooring physical duration to complete sectors.
3. MusicBrainz geometry failure degrades only to metadata text search. GNUDB has no equivalent album-text API in the existing path and therefore fails closed.
4. A rejected CUE error affects lookup only when that CUE is associated with the selection; alien rejected CUEs are ignored.
5. `CueSidecarPolicy` selects source authority before rendering in both editor-construction routes. The typed selected identity is installed on the production state and governs save routing.
6. Sidecar write-back occurs only for `MetadataCueSource::Sidecar` and only after all image writes on which the regenerated CUE depends.
7. A changed sidecar membership after editor open is a stale-source error, not an opportunity to remap or guess.
8. Album `CATALOG` is editable metadata because conversion consumes it as `CATALOGNUMBER`. Track `ISRC` and structural commands remain immutable through this path.
9. Empty/deleted metadata keeps the writer's existing omission semantics; this round updates explicit non-empty fields and does not redesign deletion policy.

## Limitations knowingly accepted

- Native multi-FILE probing remains synchronous, matching the existing ordinary Browse/GNUDB probe path. No worker architecture was introduced in this narrow round.
- Physical EOF can fall between CD-sector boundaries. The final track in that `FILE` intentionally excludes any incomplete trailing sector; authored inter-track geometry is never rounded.
- The sidecar can still change in the small interval between save-time revalidation and the existing atomic writer's read. Closing that filesystem race would require transaction/descriptor authority changes explicitly outside this round.
- Deleting an album metadata field from a CUE still follows the existing writer's omission behavior; explicit non-empty replacements are synchronized.

## Observations (not acted on)

- Several existing metadata and lookup paths perform synchronous probes on the TUI command path. This round did not introduce a new scheduling abstraction.
- Duplicate format-copy detection/selection remains intentionally deferred.
- The existing metadata transaction, journal, descriptor-authority, and crash-recovery machinery was not changed.
