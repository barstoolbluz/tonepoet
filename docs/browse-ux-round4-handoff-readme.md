# Handoff README — Browse/Picker UX Round 4 Bundle

You are the implementing model. No compiler, no repo beyond this bundle; the applying
side compiles, fixes mechanical errors, runs gates, and audits.

Read order: (1) `docs/browse-ux-round4-brief.md` — THE task; nine user items + two
carried-forward. Note the epistemics: item 6's lowercasing mechanism is PROVEN
(punctuation-blind `capitalize_word`); items 3 and 4 are REPRO-FIRST — field
observations contradict verified-correct-looking code, so build failing tests from
the user's exact inputs before changing anything, and your report must name the
mechanism you found. Item 2 (cursor) ships a four-state contrast matrix as a hard
contract with a required render test. (2) `CLAUDE.md` for conventions.

Hard constraints: NO function keys; NO emojis/decorative unicode (functional
●○✓▲▼/▸▾/█░/▌ only); Ctrl+Q stays quit; Ctrl+Shift+<x> chords are ruled out
(legacy-encoding collisions); decided chords: Ctrl+/ = deselect in editors (bind
both crossterm representations of 0x1F), Alt+A recommended select-all alternative;
Ctrl+Z/Y = undo/redo. NO undo for delete.

Bundle contents: this readme + the brief, CLAUDE.md, workspace Cargo.toml, the
complete tui-file-picker crate, src/convert/{renaming, rename_plan, source_admission,
classify, cue_parser, metadata}.rs, src/convert/pipeline/stages.rs (LARGE — included
because item 3's sanitize/template-render candidates live here; touch narrowly),
src/main.rs, and the in-scope src/tui files incl. rename_plan.rs (authoritative),
cue_parser.rs, inline_edit.rs, bookmarks.rs (item-10 tests). SHA256SUMS verifies all.
Request anything missing rather than guessing; record preimage hashes from THIS
bundle for anything you patch blind.

Deliverable: overlay tar.gz + MANIFEST.md (per-file summaries + preimages) +
ENGINEERING_REPORT.md (per-item resolution; repro-first mechanisms named; chord
choices; undo-journal design incl. the copy-undo confirmation) + SHA256SUMS.
Applying-side gate: untruncated `cargo test --workspace`, zero failures
(5088/0 baseline must not regress).
