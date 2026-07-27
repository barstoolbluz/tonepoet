# Handoff — Browse/Editor Round 5 (bundle readme)

**Governing document:** `docs/browse-round5-brief.md`. Read it fully first.
Every mechanism is audit-verified to the line (three overlapping audit bands
plus a fresh-eyes re-verification, including byte-level forensics on the real
FLAC files and a live probe of the real DVD-A ISO) — do not re-derive them,
and do not second-guess the audit-forced decisions recorded inline (menu
mechanism for deferred entries, the ≥1 presentations gate, the exact
text-engine arm span).

**Baseline:** branch `hardening` @ c12da89; `cargo test --workspace` =
5,188 passed / 0 failed across 56 targets. Version stays **0.4.4**.

## Scope, in suggested order

1. Item 1 — lowercase `go` (two sites; trivial).
2. Item 5 — Alt+L replaces the text engine's Alt+A select-all arm
   (EXACTLY text_input.rs:802-805; the adjacent Ctrl+A arm at :798-801 must
   survive byte-identical in behavior).
3. Item 2 — metadata-editor scroll off-by-one + context-menu Add-field
   reroute.
4. Item 4 — folder-level disc activation (shared-helper union arm) + the
   presentations ≥1 gate.
5. Item 6 — Copy tags submenu + session tag clipboard (full TagEntry clones
   + ordered path list); Custom/Paste surfaced as plain ENABLED
   honest-status items only.
6. Item 3 — FLAC ID3v2-prefix skip (largest item; two writer offset sites,
   recovery paths, journal format versioning, has_flac_magic consistency;
   prefix sizes VARY per file — parse per file).

## Non-negotiable constraints

- NO function keys; NO emoji/decorative unicode (functional ▸/─/► set only);
  Ctrl+Q stays quit; delete stays non-undoable; version stays 0.4.4.
- The standard/strong verification split (c12da89) is load-bearing: item 3
  operates in the NATIVE FLAC writer used by both modes; do not route
  prefixed .flac files to lofty.
- Item 3 scope fence: WRITE PATH ONLY. The library scanner and repair tool
  are a dedicated later round; the prefix-detection helper must be a clean
  reusable function.
- Item 6: Paste tags execution and the Custom builder are NEXT round. Do
  not implement them; the deferred entries emit their honest status only.
  The clipboard schema must not require changes when Paste lands.
- Do not regress: mouse text contract, 4-state cursor matrix, `:messages`,
  degraded-rename ladder, quiet status bar / close-on-success behavior.

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest (SHA-256 of
  the exact base revisions you received) covering every modified file.
- Engineering report: per-item named pinning tests, decisions taken (item 3
  overflow prefix policy — preserve-by-default recommended; item 4 gate
  change), disclosed residuals, and any deviation from the brief with
  rationale.
- `cargo test --workspace` must stay green against the 5,188/0 baseline;
  new tests must FAIL if the specific defect they pin regresses.
