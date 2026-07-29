# Handoff — Transfer Round 8 (bundle readme)

**Governing document:** `docs/transfer-round8-brief.md`. Read it fully
first. Every mechanism is audit-verified to the line (three overlapping
audit bands plus a fresh-eyes re-verification of all amendments,
including empirical tests against the user's field files). Do not
re-derive the verified seams, and do not second-guess the audit-forced
decisions recorded inline — in particular: the role-discriminated
carrier construction (`MetadataSidecar` vs `SyntheticAlbumPart`), the
track_audio_paths NUMBER-SORT at carrier construction (parse order is a
wrong-file-write trap), the dual-plan field-set rule (per-file arm =
Files plan, sidecar arm = CUE-capped plan), image-pick = PreferEmbedded
(NOT EmbeddedOnly), explicit-.cue keeps today's bypass (no resolution,
no substitution), album resolution beats gesture policy for multi-FILE
members, the four-arm gesture pin family, Space = toggle-mark+advance
with the exact anchor rule, Alt+Enter placed BEFORE the unguarded Enter
arm, the right-aligned reserved-width confirm, and the §5A/§5B field
fixes exactly as scoped.

**Baseline:** branch `hardening` @ 612a6f3 (+ docs commits);
`cargo test --workspace` = 5,295 passed / 0 failed across 56 targets.
Version stays **0.4.4**.

## Scope, in suggested order

1. §1 picker: Space toggle-mark+advance, Alt+Enter confirm, Alt+Click
   range (+ `v` visual mode, Shift aliases), right-aligned reserved
   confirm, mark-lifecycle hardening.
2. §2 multi-FILE CUE carriers: role-aware fence lift, widened carrier
   with number-sorted track_audio_paths + write-method marker,
   generalized snapshot validator, dual write for MetadataSidecar,
   sidecar-only for SyntheticAlbumPart, embedded multi-FILE
   source-only.
3. §3 gesture policy override + the routing-test pin replacement + the
   four-arm pin family.
4. §4 matrix guards: pairing corroboration, directory-embedded
   consultation, prompt arms + write-method field, Files-target
   confirm-time re-verification.
5. §5 round-7 debts; §5A APE tolerant read/write; §5B clipboard
   publish authority + tmux passthrough.

## Non-negotiable constraints

- Two-layer policy truth: admission stays policy-free; per-gesture
  policy is supplied at the resolution call sites only; the Config
  cascade setting remains FUTURE.
- CUE text is written ONLY through
  `rewrite_cue_sidecar_metadata_from_cuesheet_validated` (sidecar) or
  the compare-and-swap CUESHEET writer (embedded, FLAC-only). The
  byte-span engine itself needs NO changes — do not touch it.
- All refusals are honest statuses; no silent drops (marked-then-hidden
  files, first-of-N single-path picks, pairing misalignment — all get
  disclosure channels per the brief).
- Byobu-safe input rule: no capability reachable ONLY via F-keys,
  Shift+Click, Shift+arrows, or Ctrl+Space. Alt+Click is the primary
  range gesture; Shift variants are aliases.
- NO F-keys; NO emoji/decorative unicode (functional set only); Ctrl+Q
  stays quit; new bindings scoped to the picker surface; version 0.4.4;
  never regress `:messages`, the verification split, or rounds 5-7
  machinery (ID3-prefixed FLAC writes, tag_interchange, round-7
  carriers — the 10 non-transfer picker purposes must keep working).
- Fences: §6 list (Custom builder + Paste tags next round; config
  setting; library; disc images; SyntheticAlbumPart embedded fan-out;
  multi-FILE embedded as write target; first-track collapse on disk;
  ISRC/SONGWRITER CUE writeback; .ape/.mpc native writes).

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest (SHA-256
  of the exact base revisions received) covering every modified file.
- Engineering report: per-item named pinning tests (the brief's §7
  minimum list), the implemented carrier matrix as a table, the
  role-discriminated write-fan-out contract stated, disclosed
  limitations (lowercase-`v` type-ahead sacrifice, sheet-vs-file read
  divergence, SyntheticAlbumPart sidecar-only, embedded multi-FILE
  read-only, multi-audio-file-folder embedded limitation, .ape/.mpc
  write refusal, editor-copy pin is regression not new), and any
  deviation from the brief with rationale.
- `cargo test --workspace` stays green against 5,295/0; new tests must
  FAIL if the specific behavior they pin regresses.
