# Handoff — Corrective Round 9 (bundle readme)

**Governing document:** `docs/corrective-round9-brief.md`. Read it fully
first. Every mechanism is audit-verified to the line (three overlapping
audit bands plus a fresh-eyes re-verification, including one
forced-consensus re-derivation settled by direct read — §1.5's
mechanism section is the settled truth; do not re-litigate it). Do not
re-derive the verified seams, and do not second-guess the audit-forced
decisions recorded inline — in particular: the §1.2 neutral-row seam
(the sanctioned redesign; TUI types stay in probe.rs behind wrappers),
the §1.3 full-provenance TrackMetadata mapping, §1.5's structural
publish-root sharing with completion-order-only logging, the §2.2
extension-join hazard fix (set_extension eats a trailing dot), the
assembly-time windows_portable placement, §3's per-pane copy spec and
engine/status seam split, §4.2's new repair entry point (a changes-list
cannot name invalid keys), and §5's literal "dither: none" rendering.

**Baseline:** branch `hardening` @ 7843058 (+ docs commits);
`cargo test --workspace` = 5,322 passed / 0 failed across 56 targets.
Version stays **0.4.4**.

**The acceptance fixture is real and deliberately broken:** the user is
keeping `~/livetorrents/Supertramp – Even In The Quietest
Moments...-1977` (spec-invalid APEv2 key `&год`, preserved by round-8's
writer) untouched to verify this round in the field. §1.6 states the
exact expected outcome.

## Scope, in suggested order

1. §1.2 extraction (neutral-row seam into metadata_persistence.rs;
   probe.rs wrappers keep every round-8 pin green).
2. §1.3 pipeline routing (materializer_single primary + the corrected
   sweep inventory) + §1.4 loud-degradation contract.
3. §1.5 structural publish-root sharing for ordering-unprovable
   batches (all four pin legs).
4. §2 naming dots (four trim sites + the set_extension hazard +
   windows_portable at assembly time).
5. §3 clipboard defects (info-pane precedence, convert per-pane copy,
   engine whole-field fallback + scoped statuses, paste-prefers-shared,
   help additions).
6. §4 read-issues collapse + typed-kind threading + the repair entry
   point.
7. §5 preset disclosure (shared helper, ~6 save sites incl. the silent
   one, load-confirmation landing).
8. §6 ledger items (five-test guard, allocation clamp, consumed-flag
   gating).

## Non-negotiable constraints

- NO silent degradation anywhere: unreadable tags convert WITH a
  disclosed warning in the per-track log, reporter, and queue count.
- The four naming sanitizer edits converge on the renaming path's
  dot-preserving semantics; only exact `.`/`..` components are
  guarded; windows_portable is opt-in, trailing-only, assembly-time.
- The shared byte-span CUE engine, the round-8 write authorities, and
  the editor validators are UNTOUCHED.
- Round-8 pins stay green through the extraction — wrappers preserve
  every existing probe.rs entry point (the fence governs pin call
  paths; the neutral-row layer is the sanctioned redesign).
- NO F-keys; NO emoji/decorative unicode; Ctrl+Q stays quit; new copy
  arms scoped to their screens; version 0.4.4; never truncate gate
  output; rounds 5-8 machinery must not regress.
- Fences: §7 list (vinyl side-number parsing; pairing-guard
  relaxation; reserved-char table; `…` substitution;
  mount-capability naming; external clipboard tools; scan-at-scale
  repair; Custom builder + Paste tags; config cascade; library).

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest
  (SHA-256 of the exact base revisions received) covering every
  modified file.
- Engineering report: per-item named pins (§8's minimum list), the
  extraction module map (what moved, what wrapped), the §1.5 Mazzy
  re-derivation (which leg produced the field failure), disclosed
  limitations (leading-dot hidden dirs incl. tonepoet's own Browse
  with show_hidden off; Source-pane/batch-mode copy gaps;
  reserved-char class unchanged; repair is per-open-set), and any
  deviation with rationale.
- `cargo test --workspace` stays green against 5,322/0; new tests
  must FAIL if the behavior they pin regresses.
