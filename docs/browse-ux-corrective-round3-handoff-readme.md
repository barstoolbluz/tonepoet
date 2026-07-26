# Handoff README — Browse UX Corrective Round 3 Bundle

You are the implementing model. No compiler, no repo beyond this bundle; the applying
side compiles, fixes mechanical errors, runs gates, and audits.

Read order: (1) `docs/browse-ux-corrective-round3-brief.md` — THE task, six items,
twice-audited against this exact snapshot (hardening @ 5620d58 + the round-2 apply).
Item 3's diagnosis was audit-corrected: the real defect is comparator attribution
(destination capabilities applied to a source-owned handle at
`source_guard.rs:909`) — read it carefully. (2) The two prior briefs
(`browse-ux-corrective-brief.md`, `browse-ux-hardening-brief.md`) for context — do
not regress their green behaviors (5026/0 baseline). (3) `CLAUDE.md`.

Hard constraints: NO function keys; NO emojis/decorative unicode; Ctrl+Q stays quit;
new chords for item 1 must survive terminals that intercept Ctrl+V (Windows Terminal
is the reference environment).

Bundle: the three briefs + this readme, CLAUDE.md, workspace Cargo.toml, src/db.rs,
src/main.rs (terminal setup incl. bracketed paste), src/convert/source_admission.rs +
classify.rs, complete tui-file-picker crate, and the in-scope src/tui files (incl.
draw_footer.rs and probe.rs this round). SHA256SUMS verifies every member.

Deliverable: overlay tar.gz + MANIFEST.md + ENGINEERING_REPORT.md (per-item, incl.
the empirically confirmed item-3 route and your item-1 chord choices) + SHA256SUMS.
Request missing files rather than guessing; record preimage hashes from THIS bundle.
