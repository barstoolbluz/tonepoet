# Handoff — Transfer Carrier Semantics Round 7 (bundle readme)

**Governing document:** `docs/transfer-round7-brief.md`. Read it fully
first. Every mechanism is audit-verified to the line (three overlapping
audit bands plus a fresh-eyes re-verification of all amendments). Do not
re-derive the verified seams, and do not second-guess the audit-forced
decisions recorded inline — in particular: the two-layer admission/policy
truth, the carrier-consistency rule (same album = same carrier regardless
of selection gesture), the mark rules (directories filtered, sorted
emission), the browse-side blocking confirm, the FLAC-only embedded-write
scope, the SONGWRITER exclusion, and the write-time re-admission.

**Baseline:** branch `hardening` @ 02b8822 (+ docs commits); `cargo test
--workspace` = 5,265 passed / 0 failed across 56 targets. Version stays
**0.4.4**.

## Scope, in suggested order

1. §1 picker: `SelectedMany` completion + mark rules + contextual confirm
   with PRIORITY placement + `.cue` filter at transfer sites.
2. §2 carrier classification (both expansion seams — note the browse-side
   one is in the `launch_tag_transfer` WORKER, not the reducer arm).
3. §3 reads (CUE → Track rows via the overlay pattern; matched-sheets
   nuance).
4. §5 track-dimension planning.
5. §4 writes (sidecar composer → existing structured engine; embedded
   CUESHEET FLAC-only; field cap + key map).
6. §6 browse-side blocking confirm + statuses.

## Non-negotiable constraints

- The §0 stake: the Config cascade setting is FUTURE — thread
  `DEFAULT_FRONTEND_CUE_POLICY` at the resolution layer (never hardcode,
  never parameterize admission); a pin must prove the constant is
  swappable.
- CUE text is written ONLY through
  `rewrite_cue_sidecar_metadata_from_cuesheet` (sidecar) or the CUESHEET
  tag via the classified writer (embedded, FLAC-only this round).
- All refusals are honest statuses; no silent drops (the current
  expansion silently drops `.cue` picks — that class of behavior is what
  this round eliminates).
- NO function keys; NO emoji; Ctrl+Q stays quit; scoped keybindings;
  version 0.4.4; never regress `:messages`, the verification split,
  round-5 ID3-prefix FLAC support, or round-6 tag machinery (the 8
  non-transfer picker purposes must keep working via the compat `path`
  field — pin all 10 variants).
- Fences: config setting, library, disc images, multi-FILE CUE albums,
  ISRC writeback, range selection, Custom builder + Paste tags (round
  after).

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest (SHA-256
  of the exact base revisions received) covering every modified file.
- Engineering report: per-item named pinning tests (the brief's §6 list
  is the minimum), the carrier matrix as implemented, disclosed
  limitations (FLAC-only embedded writes, SONGWRITER exclusion,
  never-clears-CUE-fields, multi-FILE refusal), and any deviation from
  the brief with rationale.
- `cargo test --workspace` stays green against 5,265/0; new tests must
  FAIL if the specific behavior they pin regresses.
