# P0 Scope and Commission: Reference DSD→PCM Implementation Brief

**Status:** validation feedback + re-scoping on your commissioned design
(`docs/brief_dsd_reference_design.md`); your next deliverable is defined at
the end of this document
**Validation result:** the design is accepted in structure and substance. The
citation audit sampled ~35 file:line claims across all nine sections — every
one verified against the tree. Evidence fidelity is high and the fail-closed
posture is right. The design brief remains the north-star architecture; this
document narrows what gets BUILT first and corrects three technical/product
points.

## The product decision driving this re-scope

The user needs a working Auto/Reference DSD→PCM path **now**. Everything else
in the design — the Manual workflow system, its TOML schema and object store,
the in-TUI workflow builder, lossy delivery — is deferred to follow-up work.
Your P0 design must be **future-compatible** with those deferrals (no
persisted-schema break, no architectural corner) but must not implement them.

---

## P0 scope (build this)

**Reference DSD→PCM for lossless targets only, singletons only**, per the
design brief's Reference pathway:

- The full qualified chain: explicit −12 dB headroom → `rate -u` → profile
  sinc (`-a 180`, linear phase) → true-peak measurement → one constant
  restoration/compensation gain capped at −1 dBTP → one terminal realization
  → lossless packaging → verification → atomic publication.
- Immutable policy registry (`sox_ng_14_8_0_1_v1`), profiles B1–B5 with
  B4W explicit-Wideband, B6 typed-but-rejected, and the §3.3 support matrix
  with its fail-closed error cells.
- Gain modes: `Reference`, `NativeLevel`, `Fixed`, `NormalizePeak` with the
  exact §3.5 semantics and labeling.
- Lossless targets only: `FlacNative | WavRiff | WavRf64 | WavW64 |
  AiffNative | WavPackNative | AlacM4a`; depths Int16/Int24/Float32/Float64
  per the §3.8 candidate matrix. Int8/Int32 rejected as designed.
- **DST is in P0 scope** (see D2 below): DSDIFF/DST and SACD DST areas reach
  Reference through a qualified lossless-decode materialization front-end.
- Measurement/deferred-binding execution steps, fixed-point `DbNano`
  arithmetic, terminal realization bounds, fingerprint domains, manifest v2,
  serde/queue/preset migration, publication hardening, CLI/TUI/`:set`/preset
  exposure for the P0 controls, and the qualification suite — all as
  designed, restricted to the P0 surface.
- Round-1 singleton-only stands as designed (multi-member programme authority
  remains the next round after P0); the rejection message must name the
  user's options (convert as singletons with independent gain, or wait for
  programme support).

## Deferred (design for it, do not build it)

1. **Manual workflow system** (schema, admission, object store, linter,
   executor) and the **in-TUI workflow builder**. Both are confirmed future
   requirements — users must eventually design custom workflows from the
   convert screen, not only by hand-editing TOML. P0 must keep the seams:
   - `DsdSourcePathway` may keep its `Manual` variant (or equivalent typed
     reservation) so persisted settings do not need a schema break later;
     P0 validation rejects it with an actionable "not yet available" error.
   - `PipelineSettings.audio_workflow` may ship as the designed
     `Option<...>` field defaulting to `None`, or be omitted until the
     follow-up — your choice, but state it and keep the native-v2 wire
     forward-compatible either way.
   - Nothing in P0's planner/executor/publication design may assume the
     execution-step vector is Reference-only in a way that a workflow stage
     variant would later violate.
2. **Lossy delivery on the Reference front-end** (DSD→Opus/MP3/AAC through
   the qualified reconstruction chain, explicitly labeled as not-full-
   Reference). Confirmed future requirement — the P0 target enum, policy
   registry, and finalize/package seam must accommodate a later
   lossy-delivery mode without re-minting the lossless cells. Until then,
   new DSD→lossy conversions will fail closed with an error naming the
   interim options; this interim regression is accepted by the user.

## Corrections carried over from validation (apply in P0)

**D1 — the §3.2/§3.3 sinc argv is wrong by TW/2; empirically resolved.** The
design freezes `sinc -a 180 -L -t TW -PB` with PB = the passband edge. This
was measured against SoX_ng (v14.6.1 build of the same effect) with
88.2 kHz float-64 sine fixtures and a 0.5–3.5 s steady-state window:

```text
sinc -a 180 -L -t 10000 -25000  (the design's frozen B3 argv):
  20 kHz: unity    25 kHz: −6.02 dB rel    30 kHz: −187 dB rel
sinc -a 180 -L -t 10000 -30000  (corrected):
  24 kHz: unity    25 kHz: unity    30 kHz: −6.02 dB rel    35 kHz: −186 dB rel
```

The frequency argument is the **−6 dB point** (transition center). The
design's argv therefore realizes every profile narrower by TW/2 — the
frozen-B3 form is flat only to ~20 kHz with stopband from 30 kHz, while the
corrected form reproduces the evidence profile exactly (v5: unity at 25 kHz,
−6.02 dB at 30 kHz, stopband from 35 kHz). Directive: derive the argv
frequency as `PB + TW/2` for every profile (B3→`-30000`, B4→`-37500`,
B4W→`-42500`, B5→`-59000`, B6→`-114100`), state this rule and the measured
basis in the brief, and make the §6.2 #3 response qualification assert the
*profile* (flat point, −6 dB point, stopband edge), not merely the argv.
Status update: the flake now pins SoX-ng 14.8.0.1
(`barstoolbluz/sox_ng@324b8cf`), and both measurements above were re-run
identically against that exact build — the qualification suite still
re-asserts the profile, but the semantics are no longer in question.
Your §4.7 example (`-t 15000 -30000` for a "30 kHz" filter) already follows
the −6 dB-point convention; align the profile table's rendering rule with it.

**D2 — DST joins Reference via a qualified lossless-decode front-end.** The
design's blanket DST rejection is too strict and the product requires DST:

- DST is lossless compression of DSD: a correct decode yields bit-identical
  DSD, after which the chain consumes exactly what the evidence qualified —
  uncompressed DSD64/128/256.
- Tonepoet already ships a pure-Rust DST decoder **and encoder**
  (`crates/sacd-rs/src/dst/`): the decoder core is parameterized for
  DSD64/128/256 frame geometry, supports 1–6 channels, maps every upstream C
  assert to a typed error, and is validated byte-exact against
  `sacd_extract`. FFmpeg's DST decoder is an acceptable alternative pinned
  identity.
- Requirements: a qualified DST decoder identity in the policy registry
  (same immutable append-only rules); bit-exact qualification exploiting
  losslessness (decode→re-encode→frame-compare with the in-tree encoder, or
  two independent decoder identities compared, or a pinned fixture corpus —
  state which); provenance records the decode stage/identity/verification;
  the Reference label may carry "with qualified DST decode front-end";
  unattested decode fails closed. Reconcile §2.5's error text, the
  Executive-decision rejection list, `DsdSourceKind` routing, and the SACD
  Phase-1 boundary (DST decode is a per-track materialization fact and does
  not change the continuous-programme phasing). DSD512/1024 remain rejected
  (native handler limits are a separate fact).

**D3 — acceptance gates.** Scope `cargo fmt --check` and
`clippy -D warnings` to the files/crates the implementation touches; the
existing workspace does not pass the unconditional forms today.
`cargo test --workspace` and the tool-gated qualification selection remain
as stated in §6.7.

## Forward notice: PCM→DSD (do not design now)

Two future work items exist in the product plan; your P0 design should know
they are coming and avoid obstructing them, but must not design them:

1. **Auto/Reference PCM→DSD** — an evidence-qualified counterpart in the
   opposite direction (a future evidence/qualification round will define it).
2. **Custom-settings PCM→DSD** — note this largely exists today:
   `DsdFilterPreset::{Auto, Sinc}` with the explicit upsample/sinc/vol chain
   (`tonepoet-pipeline/src/plugins.rs:1559-1611`), noise shaper, modulator
   order, and trellis settings, exposed as TUI pills for DSD targets. Your
   settings split (`PcmToDsdSettings`) already models it.

Where P0 machinery is direction-agnostic for free — the policy registry
shape, `DbNano`, fingerprint/manifest domains, measurement/deferred-step
executor, qualification framework, publication hardening — prefer the
direction-neutral form and say so. Where neutrality would cost real design
effort now, keep it DSD→PCM-specific and note the seam. A short "PCM→DSD
reuse notes" section in your brief is sufficient.

## Accepted without change

Round-1 singleton-only Reference (with the improved rejection message), the
immutable policy-ID model, fixed-point dB representation, the
measurement/parser contract, serde/migration strategy, manifest v2, and the
§9 recommendations Q1–Q3/Q5/Q7–Q8. Q4 (lossy) and Q6 (Manual continuous
override) are governed by the deferral list above.

---

## Your deliverable

Author `docs/brief_dsd_reference_p0_implementation.md`: an implementation
brief **to yourself** for the P0 scope above, derived from
`docs/brief_dsd_reference_design.md` and this document. It must:

1. Restate the exact P0 surface (supported cells, rejected cells with error
   text, gain modes, targets, DST front-end) so implementation needs no
   reference back to out-of-scope design sections.
2. Apply D1: the sinc semantics are measured and closed (frequency argument
   = −6 dB point on the pinned 14.8.0.1 build). Carry the `freq = PB + TW/2`
   derivation rule and the corrected per-profile frequencies into the frozen
   argv tables, and keep the §6.2 #3 qualification asserting the profile
   response.
3. Specify the P0 type/settings subset actually built (including which
   deferred fields ship as typed-but-rejected vs. omitted) and the exact
   forward-compatibility contract for Manual and lossy delivery.
4. Order the work as independently gated sub-rounds sized to this project's
   demonstrated apply-round capacity (the P0 scope is smaller than the
   design's Round 1 but likely still 2 rounds; state the seam — e.g.
   foundation types/serde/policy/planner first, execution/qualification/
   exposure second).
5. Carry the full test plan for the P0 surface: transcript pins for every
   supported cell, rejection pins for every unsupported cell, DST
   qualification, serde/migration/sentinel tests, publication/crash tests,
   and the tool-gated selection.
6. Ground every claim about current code in file:line against the tree you
   hold. Complete-file delivery contract applies to the implementation
   rounds that follow.

Do not begin implementation in this round. Do not expand scope beyond this
document.
