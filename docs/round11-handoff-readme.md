# Handoff — Round 11 (bundle readme)

## ⛔ READ FIRST — HARD SCOPE DISCIPLINE (a prior attempt at this exact brief was rejected in full for over-engineering)

An earlier pass at this same brief was **rejected and thrown away** because it massively
over-engineered. Do not repeat it. Concretely, that attempt:

- turned **item 2a** — a folder move aborting because `renameat2(RENAME_NOREPLACE)` is
  unsupported on some mounts (a **few-line graceful-fallback** fix) — into a nine-iteration
  "protocol-v3 intent-recovery" crash-safe move/copy **transaction system**: append-only
  ownership/completion journals, exact device/inode ownership claims, completion-less
  adoption recovery, group-commit batching, open-handle metadata-fidelity proof trees,
  symlink-atomic copy, `_v8`/`_v10` publication generations, ~37 adversarial tests; and
- rebuilt the **tag-write path (items 4/7)** with an ABA/decoy-inode-substitution-resistant
  "conditional metadata commit" system (~285 lines, ~11 tests) defending against a
  destination file being swapped for a decoy inode mid-commit.

Both solved problems **that do not exist in this domain. tonepoet is a single-user desktop
audio TUI** — not a distributed database, not a hostile multi-writer filesystem. There is no
adversary swapping inodes; there is no fleet; move-during-crash data loss has never been
reported. The whole delivery was discarded and this brief was re-issued unchanged except for
this section.

**Non-negotiable rules for this round:**

1. **Implement exactly the behaviour the brief scopes — and nothing more.** A fix must be
   *proportionate to the bug*. "Graceful fallback" means write the fallback, not a protocol.
2. **No new subsystems, protocols, journals, transaction managers, ownership/adoption
   schemes, or crash-recovery/durability layers** unless the brief explicitly asks for one.
   It does not ask for any.
3. **No defenses against threats this domain doesn't have** — no ABA/decoy-inode resistance,
   no adversarial-race hardening, no Byzantine anything.
4. **Do not harden arcane edges** (standing rigor-vs-usability directive). Default path =
   fast + graceful degrade. A rare, non-catastrophic edge should degrade gracefully or be
   left alone.
5. **If you catch yourself building a "system," STOP.** The smallest change that satisfies
   the stated outcome, in the surrounding code's style, is the correct answer.
6. **Being the stronger reasoner means finding the SIMPLEST correct solution, not the most
   elaborate.** Robustness is not complexity. The "How to read this brief" empowerment note
   below still holds — but it is licence to pick a *better, simpler* root cause, never
   licence to gold-plate. If you genuinely believe something needs more hardening than the
   brief asks, **do not build it** — add one sentence flagging it for the user to decide in a
   future round.
7. **Size check:** the whole round should be a *modest* diff across the listed files. If any
   single item is adding thousands of lines and dozens of tests, you have gone wrong —
   re-read this section and cut back.

---

**Governing document:** `docs/round11-brief.md`. Read it fully first — including its
"How to read this brief" note. In short: the root-cause analyses and `file:line`
anchors are *findings to save you search time*, not prescriptions. You are the stronger
reasoner. If your own analysis discloses a more likely — or more fundamental (Ur-) —
root cause than what the brief records, trust your findings and follow them; say so and
proceed. The "what should happen" notes describe desired behaviour, not required
implementation. Push back on anything wrong.

**Baseline:** branch `hardening` == `main` @ `90c2b96` (rounds 8–10 merged, audited).
`cargo test --workspace` = 56 targets, **5384 passed / 0 failed / 15 ignored** (inside
`nix develop`, never plain `cargo test`). Version stays **0.4.4**. Preserve the green
suite: new behaviour needs pins; changed behaviour needs its pins updated, not deleted;
never truncate gate output.

**No compiler on your side** — expect Claude Code to fix compile-scope issues (exhaustive
struct literals, out-of-bundle field-count sentinels, new enum-variant matches) on apply.
Return full modified files + an engineering report describing each change and its pins.

## Scope — eight items (details + anchors in the brief; suggested grouping, not binding)

1. **Verify/close-out the APE numbering work** (Part A A5). Two non-blocking follow-ups:
   `LoftyApe` numbering has no round-trip test; a lofty ID3v1 truncation panic (external
   dep) ties into Item 7's guard.
2. **Cut/paste directory move + undo/redo.** The move refusal is `renameat2(RENAME_NOREPLACE)`
   reporting `Unsupported` on filesystems lacking the flag — needs graceful degradation, not
   a "replacement." Move undo/redo = deterministic reverse-replay with an invalidation guard
   (no-op if the moved item was mutated first). Plus text-field undo/redo — missing at every
   inline editor; the shared `tui-file-picker` `text_input` engine is the single choke point.
3. **Surface integer vs float sample format** (32i vs 32f, 32f vs 64f). The TUI probe drops
   `sample_fmt`; the pipeline already classifies it.
4. **"Remove all tags"** — bottom entry of the context-menu Tags & Tagging submenu (separator
   above) and of the overlay tags-button popup; both with a yes/no confirmation.
5. **Maximizable metadata-editing overlay** — reuse the convert-pane maximize/collapse pattern
   (▸/▾ + double-click title bar).
6. **Canonical/All "View" selector** in the overlay; "All" maximizes and lists every embedded
   tag.
7. **"Repair tags"** — partly built already (invalid-APE-key removal exists but is buried);
   surface it discoverably under Tags & Tagging + the tags popup, extend it to strip legacy
   ID3v2-before-`fLaC` prefixes (detection helper exists; writes currently *preserve* the
   prefix), add the char-boundary guard for the lofty ID3v1 panic, no-op when clean.
8. **foobar "Optimize file layout" is padding/layout hygiene, not tag repair** — recommend
   *not* folding into Item 7; optional standalone Utilities item if wanted.

## Non-negotiable constraints

- **Input:** NO F-keys ever; byobu-safe (don't make Ctrl+Space / Shift+Click / arrows the
  only path; range-select primary = Alt+Click); Ctrl+Q stays quit. No emoji / decorative
  unicode (the ▸/▾ pane indicators are the sanctioned exception).
- **Do not regress rounds 5–10** (all on `main`, audited): the §1–§6 metadata-authority /
  DSP-honesty / numbering machinery, the DSD Reference qualification path (Int32 stays
  fail-closed; policy dither), and the fingerprint contracts (no existing digest changes for
  non-explicit settings).
- Version stays 0.4.4.

## Fences (still queued from prior rounds — do NOT fold in unasked)

- Custom tag builder + Paste tags (user has mockups; queued).
- Pairing-guard relaxation (awaits user field evidence).
- The Utilities-menu scan-at-scale for ID3-prefixed FLACs is the *batch* counterpart to
  Item 7's single-file repair — Item 7 is the per-file/album case only.

## Acceptance fixture (live, relevant to Item 7)

The user keeps `~/livetorrents/Supertramp – Even In The Quietest Moments...-1977`
deliberately broken (invalid APEv2 key `&год`). Item 7's "Repair tags" should cleanly fix
that file; do not "repair" the fixture in tests — it is the field acceptance instrument.
