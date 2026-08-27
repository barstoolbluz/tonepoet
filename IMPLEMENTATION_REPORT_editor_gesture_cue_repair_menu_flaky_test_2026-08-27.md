# Implementation report — editor gesture, Repair CUE menu, XDG test race, Format-pane layout, and DSF timing authority

Date: 2026-08-27

Baseline specified by work order: `main` @ `e5e4bd5` (v0.4.9).

## Result

All seven requested outcomes are implemented in the supplied source tree. The changes were kept local to the existing metadata-editor routing, Browse classification/enrichment path, test-only DB path resolution, Format-pane model/render/hit registration, and standalone DSD timing inspection/materialization paths.

The mandatory Nix workspace gate could **not** be executed in the supplied execution sandbox: `nix`, `cargo`, `rustc`, and `rustfmt` are not installed, and no Nix profile/toolchain is present. I did not substitute a non-Nix build and I do not represent the bundle as compiler- or gate-verified. See **Verification** below.

## 1. Default TITLE gesture opens the per-track view without starting an edit

`metadata_editor_begin_detail_edit_for_entry_inner` now separates two behaviors that were previously coupled by one boolean:

- move the detail cursor to the first writable slot;
- begin an inline edit in that slot.

Enter and value-column double-click use `move_to_first_writable_slot = true`, `begin_edit = false`. The detail view therefore opens with its bottom-row controls immediately usable, while a blocked first row does not become a new dead-end: the cursor lands on the first writable row. A second Enter in the detail view remains the explicit value-edit gesture.

The existing post-add-field route still requests both cursor movement and immediate editing. The explicit inline grid-cell route is unchanged. Key-column double-click retains its pre-existing inline behavior.

Coverage added/strengthened:

- mixed and uniform TITLE default gestures assert `detail_edit.is_none()`;
- single-slot TITLE default gesture asserts no inline edit;
- blocked-first-slot TITLE asserts the cursor lands on the next writable slot without editing;
- value-column double-click asserts no inline edit;
- the existing unchanged-value idempotency behavior remains covered after an explicit second-Enter value edit.

### Decision

When row 0 is blocked, the per-track view lands on the **first writable slot**, but does not start editing it. This keeps the default gesture useful without surprising the user with an active text session.

## 2. Repair malformed CUE is available from the Browse context menu

`FolderContentClassification` now carries a `CueRepairAvailability` tri-state:

- `Unknown`;
- `Absent`;
- `Repairable(PathBuf)`.

Normal Browse classification remains cheap. An explicit context-menu availability request enriches the existing cached classification on the existing bounded worker. That worker performs the CUE inspection; the reducer/context-menu builder never parses a CUE synchronously.

For a repairable cumulative cross-file CUE, the directory context menu exposes:

`Repair malformed CUE (create copy)`

Unresolved availability is shown disabled while enrichment is pending; known-absent repairability is omitted. The action calls the same `repair_cross_file_cumulative_cue` implementation already used by the CUE chooser. It therefore preserves the existing contract: create/reuse a validated repair copy; never mutate the malformed original.

Coverage includes a cumulative multi-file CUE that exposes the enabled action and a valid sidecar CUE that does not.

### Decision

I chose **cached tri-state repairability resolved on the existing enrichment worker**. This matches the Browse responsiveness architecture and avoids an action that is normally inert, without adding a second discovery subsystem or reducer-thread parsing.

## 3. XDG_DATA_HOME flaky-test class is fixed without suite-wide serialization

The root problem was that `db_path()` read process-global `XDG_DATA_HOME`, so an unguarded libtest thread could observe a temporary data home owned by a different guard-holding test and then race that test's teardown.

Under `cfg(test)`, `db_path()` no longer derives its path from process-global XDG state:

- the guard-owning thread receives a thread-local isolated data-home override;
- unrelated test threads resolve to one stable, process-local temporary test data home;
- production builds continue to use `dirs::data_dir()` exactly as before.

`XdgConfigHomeGuard` still owns/restores the real environment variables for code that genuinely needs them, and the established coordination -> XDG guard order is untouched. There is no retry, `#[ignore]`, or suite-wide serialization.

Regression coverage holds an XDG guard open while an unguarded sibling thread calls `db_path()`, proving that the sibling does not borrow the active guard's temporary data directory and that the owner still gets its isolated journal path.

### Scope check

I specifically checked the isolated metadata-journal helper and its DB-path usage before considering a broader propagation mechanism. The helper opens the journal synchronously on the guard-owning test thread; I found no evidence that these tests require the isolated DB override to propagate into spawned workers. Adding process-wide ownership machinery would recreate the same class of shared-state hazard and was therefore rejected.

## 4 + 5. Format-pane rows now have one dynamic layout authority

The hardcoded hit-region row offsets and the independent static keyboard-row arrays are removed for the Format pane. `FormatState::pane_rows(maximized)` is now the single ordered row model consumed by:

- rendering;
- keyboard focus traversal;
- mouse hit-region registration;
- default Format-pane height computation.

This fixes the shipped DSD->PCM offset regression and removes the structural opportunity for a future added/removed row to silently desynchronize those three surfaces.

`FormatField` now includes the previously below-the-fold `Container` and `ResampleQuality` rows, making them first-class keyboard stops. Their mouse handling is routed through the same centralized Format interaction path as the other fields.

Dynamic DSD gain rows:

- `disabled`: only the DSD gain mode row is shown;
- `manual` / `Fixed`: `gain dB` is shown; gain scope and auto margin are absent;
- `auto`: gain scope and auto margin are shown; gain dB is absent;
- future promoted `NormalizePeak`: gain scope and normalize target are shown.

A hidden dynamic row has no keyboard stop and receives no hit region. If a mode change hides the currently focused field, focus returns to `Format`. Collapsing a maximized pane similarly clears a below-the-fold focus that is no longer visible.

The resampling quality row exposes `insane` only for resamplers that support it; switching away from such a resampler clamps a staged `Insane` value to `Ultra` rather than retaining a value the new row cannot represent.

### Item 5 decision

Gain scope is shown only for **automatic normalization modes** (`Auto`, and `NormalizePeak` once that policy is promoted). A fixed/manual gain has no normalization peak to scope, so showing a scope selector there would be semantically inert.

### Render-to-buffer coverage

`dsd_to_pcm_render_keyboard_and_hit_map_share_one_dynamic_layout` renders the actual Convert screen through Ratatui's `TestBackend`, locates rows from the rendered buffer, then checks registered hit regions against those rendered coordinates. It covers:

- gain scope;
- auto margin;
- container;
- resampling quality / `insane`;
- absence of the inactive gain-dB row in Auto mode;
- presence/coordinate of gain dB and absence of scope/auto-margin in Fixed mode;
- keyboard visibility of the same fields;
- an actual gain-scope click, proving it changes scope without silently switching the gain mode to manual;
- below-the-fold container and quality interactions.

The same shared model also corrects the currently policy-gated Reference branch instead of leaving a known-offset bug dormant there.

## 6. Permanently unavailable DSD gain modes are not presented as clickable choices

The fail-closed Reference policy is unchanged. `reference`, `native`, and `normalize` remain disabled by policy in an ordinary build and are now omitted from the rendered DSD-gain pill instead of being displayed as permanently greyed choices.

The underlying options/settings plumbing remains intact. If/when the native-v2 Reference policy is promoted, the existing enablement gate makes those choices visible without a second UI rewrite.

### Decision

Omit permanently unavailable choices from the interactive pill. This follows the same “do not present inert controls” rule as item 5 while preserving promotion plumbing.

## 7. DSF declared sample timing remains authoritative across unrelated payload diagnostics

`DsdPlannerSourceMetadata::authoritative_sample_timing()` now treats DSF timing fields independently from unrelated container diagnostics:

- declared sample count must exist and be non-zero;
- sample rate must be a supported DSD rate;
- DSF sample count must be byte-aligned because the reader is byte-granular;
- an unrelated DSF payload-size diagnostic no longer demotes otherwise usable declared timing.

Materialization now asks one shared helper for authoritative standalone DSD timing. For DSF, genuinely unusable exact header timing is an explicit materialization error rather than a silent fallback to ffprobe's block-padding-inclusive estimate. The post-encode tolerance is unchanged.

DSDIFF behavior is intentionally narrower: clean DFF/DSD and DFF/DST continue to use exact container timing, but a DSDIFF validation error retains the pre-existing ffprobe fallback rather than inheriting the new DSF fail-loud rule. This avoids changing a second container family merely because item 7 happens to share the DSD pipeline.

Coverage creates a real DSF fixture, appends an 8192-byte payload overhang, patches the DSF chunk sizes, confirms the inspector reports an error, and then proves the declared `(2_822_400 Hz, 16_384 samples/channel)` timing remains authoritative. A second fixture patches the DSF declared sample count to a genuinely unusable non-byte-aligned value and asserts that the padded-duration ffprobe fallback is refused.

### Decision

Header authority is **field-specific for DSF timing**, not “all diagnostics must be clean.” Payload-overhang diagnostics remain visible, but they do not invalidate an intact rate/sample-count declaration. DSDIFF retains its previous error-free authority rule.

## Other tree findings

I found no material contradiction between the brief's reported roots and the supplied tree. Two implementation refinements were important:

1. Item 1 could not safely be a one-boolean flip because that boolean also controlled first-writable cursor placement; the two semantics were separated.
2. Item 7's diagnostic exemption was deliberately limited to DSF after integration review. Extending the same policy/fail-loud behavior to DSDIFF would have been broader than the field failure and brief justify.

## Verification

### Required Nix gate — NOT RUN in this sandbox

The work order requires all build/test activity inside:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

This sandbox does not contain `nix`, `cargo`, `rustc`, or `rustfmt`, and no usable Nix profile/toolchain is installed. Therefore:

- I cannot truthfully report the workspace gate green;
- I cannot truthfully report compiler-warning status;
- I did not run a plain/non-Nix Cargo substitute.

The handoff gate should run the exact commands above and require every `test result:` line to report `0 failed` on both runs.

### Static checks completed here

- compared the corrected tree against the supplied baseline copy;
- scanned all 17 changed Rust files with a comment/string-aware delimiter checker: balanced;
- checked changed Rust files for merge-conflict markers: none;
- searched the source tree for the removed `FormatField::visible_rows` API: none;
- searched for obsolete zero-argument `focus_next()` / `focus_prev()` Format calls: none;
- checked every `FolderContentClassification` literal for the new repairability field: complete;
- traced the CUE repair action back to the existing repair implementation;
- traced Format rendering, keyboard traversal, and hit registration to the shared dynamic row model;
- verified the new regression-test sources are present for items 1, 2, 3, 4/5/6, and 7.

## Files changed

Core changes are in:

- `src/tui/keybindings.rs`
- `src/tui/browse.rs`
- `src/tui/context_menu.rs`
- `src/tui/app.rs`
- `src/tui/draw_output.rs`
- `src/tui/convert_screen.rs`
- `src/tui/button_map.rs`
- `src/tui/format_interactions.rs`
- `src/tui/test_support.rs`
- `src/db.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/materializer_single.rs`
- `src/convert/pipeline/materializer_archive.rs`

Additional small compile-shape/documentation updates accompany the new `FolderContentClassification` field in existing test/fixture sites and message documentation.
