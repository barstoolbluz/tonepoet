# Revision Request: DSD Reference Design (docs/brief_dsd_reference_design.md)

**Status:** validation feedback on your commissioned design; revise the design
brief and return it before implementation begins
**Validation result:** the design is accepted in structure and substance. The
citation audit sampled ~35 file:line claims across all nine sections — every
one verified against the tree. Evidence fidelity is high and the fail-closed
posture is right. The items below are the complete set of required revisions;
nothing else in the design needs to change unless a revision forces it.

Deliverable: an updated `docs/brief_dsd_reference_design.md` (same file,
revised in place) plus a short changelog section at the top listing what
changed in response to each item R1–R7.

---

## R1 (required, product): in-TUI workflow authoring

§5.1 exposes Manual mode only as a *picker* over `.toml` files the user
hand-authored in `<config-root>/tonepoet/workflows/`. That cannot be the only
authoring path. Users must be able to design a workflow from inside the
convert screen.

Requirements:

- An in-TUI workflow builder reachable from the Format pane when
  `pathway = Manual` (e.g. the workflow row offers `new…`/`edit…` beside the
  existing named selection). Surface additional Format-pane rows or an
  overlay as needed — follow the existing pill/`FormatField` navigation
  architecture and the two-pass draw/button-map convention
  (`src/tui/app.rs:2938-2993`), and the established overlay patterns for
  multi-step editing (the theme builder's tabbed overlay model is prior art).
- The builder edits the same objects the schema defines: ordered stages with
  backend, argv tokens (one token per entry, whole-token placeholders),
  `output_extension`, and the output contract; top-level dependency
  declarations; `continuous_source_handling`; `unclassified_option_policy`.
- Saving writes a canonical schema-v1 TOML file into the workflows directory
  and immediately runs the full §4.3 admission (stable-read, canonicalize,
  hash, dependency snapshot) plus the §4.6 linter, surfacing findings in the
  builder before the workflow is selectable. There is exactly one persistence
  format: the builder is a front-end to the existing file schema, never a
  second serialized form.
- Editing an existing workflow re-runs admission and re-hashes; queued items
  holding the old snapshot are unaffected (the snapshot-on-queue invariant in
  §1.6 already guarantees this — state it explicitly for the builder).
- Builder-created workflows receive no trust shortcut: the §4.1 first-use
  acceptance flow may be streamlined for a workflow the user just authored in
  this session, but the admission checks themselves are identical.
- Specify the exposure in the same detail as §5.1–§5.6: field rows, overlay
  layout, `:set`/command-mode equivalents if any, mouse targets, and tests
  (render-buffer pins for the builder, admission-on-save, lint surfacing,
  no-second-format).

## R2 (required, product): DST belongs in Reference via a qualified lossless-decode front-end

The design rejects all DSDIFF/DST and SACD/DST from Reference v1 (§2.5,
Executive decision) on the grounds that the original decoder is not the
qualified native SoX reader. That is too strict, and the product requires DST
support:

- **DST is lossless compression of DSD.** A correct decode yields
  bit-identical DSD samples, after which the chain consumes exactly what the
  evidence qualified: uncompressed DSD64/128/256.
- Tonepoet already ships a pure-Rust DST decoder **and encoder**
  (`crates/sacd-rs/src/dst/` — decoder core is rate-generic; every upstream
  C assert maps to a typed error), used today by SACD extraction
  (`src/convert/pipeline/stages.rs:2045-2112`, which your §0.2 already
  cites). FFmpeg also carries a DST decoder as a fallback identity.

Directive: add a qualified **DST materialization stage** to the Reference
pathway. DSDIFF/DST sources and SACD DST areas decode to an uncompressed DSD
carrier (DSF or uncompressed DSDIFF) *before* the Reference chain; the
existing §2.2 verified-materialization machinery then treats the decoded
carrier as `IN`. Design requirements:

- The policy registry gains a qualified DST decoder identity (the in-tree
  sacd-rs decoder is the primary candidate; a policy-pinned FFmpeg decode is
  an acceptable alternative identity). Same immutable append-only rules.
- Qualification must exploit losslessness: bit-exact verification — either
  decode→re-encode→frame-compare using the in-tree encoder, or decode via two
  independent decoder identities and compare, or a pinned fixture corpus with
  known-good decoded digests. State which, and add it to the §6.2 table.
- Provenance records the DST decode stage, decoder identity, and
  verification result. Wording: the full Reference label may state "with
  qualified DST decode front-end"; an unattested or unverified decode fails
  closed exactly like any other unqualified cell.
- Reconcile: the §2.5 DST error text, the Executive-decision rejection list,
  `DsdSourceKind` handling in §2.1 (the enum already models `DsdiffDst` and
  `SacdArea { Dst }` — routing changes, not the type), the §3.3 matrix
  framing, and the SACD Phase-1/Phase-2 boundary (§2.6): DST decode is a
  per-track/per-area materialization fact and must not silently change the
  continuous-programme phasing.
- DSD512/1024 remain rejected as designed (native handler limits are a
  separate fact from DST).

## R3 (required, technical): pin the sinc frequency-argument semantics

§3.2/§3.3 freeze render argv with the profile **passband edge** as the sinc
frequency argument (`sinc -a 180 -L -t TW -PB`, e.g. `-t 10000 -25000` for
B3). SoX documents the `sinc` frequency argument as the −6 dB point. If that
holds for SoX-ng 14.8.0.1, the frozen argv realizes a response centered at
the passband edge — flat only to ~PB−TW/2, stopband from ~PB+TW/2 — i.e.
every profile lands narrower by TW/2 than the evidence profiles (v5 measured
unity at 25 kHz and −6.02 dB at 30 kHz for the DSD64 profile). Realizing B3
as specified would then need `-30000` (transition center), not `-25000`.

Directive: state the empirically verified semantics of the frequency argument
under the pinned build, derive the argv frequency from the profile
accordingly (either `-PB` or `-(PB + TW/2)`), and make the §6.2 #3 response
qualification assert the *profile* (passband flat point, −6 dB point,
stopband edge), not merely the argv. Your own §4.7 worked example uses
`-t 15000 -30000` for a "30 kHz" filter — the design must not leave the two
conventions coexisting ambiguously.

## R4 (decided, product): keep DSD→lossy working — add a Reference-front-end lossy mode

Q4's recommendation (Reference v1 rejects lossy; users write Manual
workflows) regresses a shipping capability: new DSD→Opus/MP3/AAC conversions
would have no built-in path at all once the legacy chain closes to new work
(§8.1). Decision: **adopt the future mode Q4 itself sketches, in Round 1.**

- Mode: the full Reference chain through measured constant gain (headroom,
  `rate -u`, profile sinc, true-peak measurement, ceiling) followed by a
  policy-listed lossy encoder stage consuming the float carrier. Specify the
  encoder hand-off precisely: which carrier (R64 after gain vs. a defined
  intermediate), no integer quantization or dither before a lossy encoder
  (the encoder owns its internal quantization — align with v9 §7's "a later
  encoder may only package or losslessly encode" by making this an explicitly
  *distinct, labeled* mode, not full Reference), and which encoder settings
  flow in (the existing codec settings: Opus bitrate/complexity, MP3 mode,
  AAC bitrate).
- Labeling: "Reference reconstruction front-end; lossy delivery" — never the
  full Reference qualification label; provenance and log wording per §3.9
  discipline.
- Targets: the lossy members of `ResolvedOutputTarget` already enumerated in
  §1.1. Qualification: transcript pins + decode-probe sanity (rate/channels/
  duration), not sample equality (impossible for lossy); state this in §6.2.
- Int32 stays rejected per Q5. Accepted as designed.

## R5 (decided, product): Round-1 singleton-only Reference stands

P1 is accepted as designed: Round 1 rejects multi-member Reference-front-end
batches, Round 2 ships programme authority. No change requested — recorded
here so the revision does not re-open it. Ensure the Round-1 rejection
message tells the user their recoverable options (convert as singletons with
independent gain, choose Manual, or wait for programme support).

## R6 (required, logistics): sub-phase Round 1

Round 1 as scoped (settings model + hand-written serde + policy registry +
three fingerprint domains + manifest v2 + measurement/deferred executor +
complete workflow system with content-addressed object store and GC + full
exposure + Cartesian qualification) is 2–3 apply rounds of work at this
project's demonstrated round sizes. Split Round 1 into independently gated
sub-rounds with explicit seams, each leaving the workspace green and
shippable. Suggested cut (adjust as you see fit, but state the seams):

1. **R1a:** types/settings/serde/migration, policy registry, fingerprint
   domains, manifest v2, planner operations + validation/matrix errors,
   `SourceInfo` extension. All new routes still planner-rejected
   ("implementation pending") — pure foundation, fully tested.
2. **R1b:** execution steps (measurement/deferred binding), Reference
   render/measure/finalize/package + DST front-end, publication hardening,
   singleton end-to-end, tool-gated qualification for enabled cells.
3. **R1c:** workflow system (schema, admission, object store, GC, linter,
   executor), in-TUI builder (R1), CLI/TUI/`:set`/preset-v4 exposure,
   lossy-delivery mode (R4), wording/provenance, final qualification report.

## R7 (required, logistics): calibrate the acceptance gates

§6.7 requires `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings`. The
existing workspace does not pass those today. Scope both gates to the
files/crates the implementation touches (or specify an explicit pre-cleanup
sub-round with its own audit); the unconditional workspace-wide form would
fail on pre-existing code unrelated to this work. `cargo test --workspace`
and the tool-gated selection remain as stated.

---

## Out of scope for this revision

Q1–Q3 and Q6–Q8 recommendations are accepted as written. The immutable
policy-ID model, fixed-point dB representation, measurement/parser contract,
programme-authority design, serde/migration strategy, and manifest v2 are
accepted without change. Do not expand scope beyond R1–R7.
