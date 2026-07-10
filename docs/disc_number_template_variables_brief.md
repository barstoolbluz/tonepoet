# Brief: user-authored disc folder naming via disc-number template variables

Date: 2026-07-09

## Correcting the record first

The current disc-folder naming contains a rule the user never asked for.
`disc_folder_name_from_source_path_style` (stages.rs:17677) names the
output disc folder after the SOURCE disc directory's style — prefix casing
and separator preserved, digits normalized to two — so a source tree with
`CD 1`/`CD 2` (real case: the WarGames OST rip) produces output folders
`CD 01`/`CD 02`. The user's actual instruction in the earlier disc-subfolder
round was to **standardize on `disc NN`** (lowercase). Source-style
preservation was invented, not requested. It makes the output library's
disc naming inconsistent across albums: it inherits whatever convention
each ripper used.

## The feature (user-specified)

Instead of hard-coding any single convention, expose the disc number as
template data and let the user author the naming, exactly like every other
template variable:

- `%DISCNUMBER%` — the disc number, unpadded (`1`, `2`, `12`)
- `%NNDISCNUMBER%` — zero-padded to two digits (`01`)
- `%NNNDISCNUMBER%` — zero-padded to three digits (`001`)
- Consider `%DISCTOTAL%` too (unpadded) so users can write `CD 1 of 2`;
  cheap if the plumbing is there, skip if it complicates anything.

Usable in both folder and filename templates; the literal prefix is the
user's text:

```
%ALBUM% (CD%DISCNUMBER%)                                  # 'CD' user-entered
%ARTIST% - %ALBUM% (Disc %DISCNUMBER%) [%CATALOGNUMBER%]  # 'Disc' user-entered
disc %NNDISCNUMBER%/%TRACKNN% - %TITLE%                   # lowercase house style
```

Note the second and first examples put the disc designator in the ALBUM
FOLDER name (per-disc album directories) — that is a legitimate,
deliberate library layout some users want, and it must work, not be
prevented.

## Semantics to design (the load-bearing decisions)

1. **When do the variables render empty?** Single-disc albums routinely
   carry `DISCNUMBER=1` tags; a template like `{disc %NNDISCNUMBER%/}`
   must NOT produce a `disc 01/` folder for them. The variables should
   render empty (so the existing conditional `{...}` blocks drop the
   whole group) unless the album is a **proven multi-disc set** — the
   same evidence bar `%DISC_FOLDER%` uses today
   (`source_has_proven_multi_disc_layout` + `track_disc_number_hint`,
   which includes path hints and the v10 batch-scope identity, so
   tag-silent tracks in a `disc N` source tree still resolve). Whether
   the `create_disc_subfolders` switch ALSO gates the new variables is
   your call — our recommendation is NO, and the existing mechanics
   support it naturally: the switch never gates any token's VALUE today.
   It works by template projection — `naming_template_with_disc_subfolder`
   (src/convert/formats.rs:603) prepends `%DISC_FOLDER%/` to the filename
   template when the switch is on and the token is absent; the token
   itself renders empty purely on the multi-disc evidence. Keeping the
   new variables as evidence-gated data (switch-independent) is
   consistent with that. Decide and document; single-disc emptiness is
   the hard requirement either way.
2. **`%DISC_FOLDER%` becomes the standardized convenience.** Default
   value `disc NN` (lowercase — the user's stated house standard).
   **Remove source-style preservation** (`disc_folder_name_from_source_path_style`
   and `styled_disc_component_name`'s use for output naming) along with
   its tests (`disc_folder_token_preserves_source_disc_directory_naming_style`,
   stages.rs:23405, et al). The token's emptiness rule (multi-disc
   evidence) and the switch's prepend projection stay as today.
3. **Reconcile the existing `%DISC%` token** (stages.rs:17547). Today it
   renders `template_disc_number` (stages.rs:17733) which is the disc
   hint `.unwrap_or(1)` — so `%DISC%` renders `1` for single-disc albums
   and is NEVER empty. The new variables' empty-when-single-disc rule is
   therefore a real semantic difference, not an alias. Options: make
   `%DISCNUMBER%`
   its gated alias and deprecate/keep `%DISC%` for back-compat with the
   same new emptiness semantics, or leave `%DISC%` untouched as raw data.
   Decide, document, and cover the choice with a test. Do not silently
   change what existing user templates render for single-disc albums
   without saying so in the implementation notes.

## The trap: companion routing must follow the RENDERED paths

Nested companion copying today re-derives the destination disc directory
name from the SOURCE directory name (stages.rs:14844-14852:
`disc_number_from_template_component_name` → `styled_disc_component_name`
→ fallback `Disc {disc:02}`). That is parallel logic to the template
renderer, and with user-authored disc segments it will diverge — e.g. the
template renders `(CD1)` per-disc album dirs while companions land in a
`disc 01/` subfolder that no audio lives in.

Companion artifacts from a source disc directory must land in the
directory where THAT DISC'S AUDIO actually rendered — derived from the
planned/published output paths of that disc's tracks, not from an
independently recomputed name. Treat this as a named requirement with its
own acceptance, exactly like the multidisc-identity round did. The batch
companion coordination under the publish lock (v10) is the machinery that
knows both the source disc dirs and the per-track final paths.

## Hard constraints

- Default behavior ships as the user's standard: with disc subfolders on
  and no custom disc variables in templates, output disc folders are
  `disc 01`, `disc 02` — lowercase, two digits — regardless of source
  directory style. (This changes today's output for `Disc`-styled and
  `CD`-styled sources; that is the point. Update affected tests.)
- Conditional `{...}` template blocks must interact correctly with the
  new variables (empty variable drops the block — the existing rule).
- Deterministic: same tree + template → same layout, re-runs identical.
- Multi-disc evidence rules are UNCHANGED — this brief is about naming,
  not detection. Do not touch `source_has_proven_multi_disc_layout`,
  batch identity, or disc-number hints beyond consuming them.
- Validation: `validate_template` remains structural-only; unknown tokens
  still resolve at render time (`%NNDISCNUMBER%` must not break older
  builds' templates conceptually, but no cross-version work needed).
- Update user-facing template documentation wherever variables are listed
  (search the TUI and docs for `%DISC_FOLDER%` mentions). The production
  default filename template literal `"%NN% - %TITLE%"` is passed to
  `effective_naming_template` at src/tui/convert_actions.rs:567 and
  src/tui/command.rs:5270; the Output Options display default lives in
  src/tui/app.rs:3723. None of those should need to change for the
  default acceptance case — the standardization happens inside
  `%DISC_FOLDER%`'s value.
- Presets carry templates as opaque strings — no preset schema work
  expected; confirm round-trip.
- Suite baseline: 2682 lib tests passing, 0 failures, zero warnings on
  cold builds (cargo suppresses warnings for cached crates). The
  tui-file-picker crate is separate and untouched.
- The sandbox cannot compile; the applier fixes compile errors and runs
  the real-tree acceptance. Favor mechanically verifiable changes; state
  intended behavior per semantic decision in tests.

## Acceptance (real trees, run by the applier)

Source: `~/livetorrents/WarGames (Original MGM Motion Picture Soundtrack)
- Arthur B. Rubinstein {Quartet Records - QR352} (2018) [CD]/` with
`CD 1/`, `CD 2/`, `Scans/`.

1. Default templates, disc subfolders on → ONE album dir with `disc 01/`
   and `disc 02/` (lowercase standard; NOT `CD 01`), all tracks, 0
   failures; per-disc companions in their own disc folder exactly once;
   `Scans/` copied per companion-folder rules.
2. Filename template `disc %NNDISCNUMBER%/%TRACKNN% - %TITLE%` →
   identical layout to (1) (variables reproduce the standard).
3. Folder template ending `%ALBUM% (CD%DISCNUMBER%)` → per-disc album
   directories `... (CD1)` and `... (CD2)`; each disc's audio AND its
   companions land in its own directory; no stray `disc NN` folders.
4. A single-disc album with `DISCNUMBER=1` tags and template
   `{disc %NNDISCNUMBER%/}%TRACKNN% - %TITLE%` → no disc folder.
5. Re-run each twice → byte-identical layouts.

## Files in this bundle

- `docs/disc_number_template_variables_brief.md` — this brief
- `docs/multidisc_identity_brief.md` — prior art for batch identity +
  companion coordination (context)
- Pipeline: `src/convert/pipeline/{stages.rs, types.rs, unified_request.rs,
  materializer_single.rs, materializer_cue.rs}`
- Conversion domain: `src/convert/{formats.rs, processor.rs, mod.rs}`
- TUI: `src/tui/convert_actions.rs` (default templates), `src/tui/presets.rs`
- CLI: `src/main.rs`
