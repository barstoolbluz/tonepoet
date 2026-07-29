# Round 10 — Metadata Write Authority, DSP Honesty, Numbering Fallbacks

**Baseline:** branch `hardening` @ 14c9b04; `cargo test --workspace` =
5,354 passed / 0 failed across 56 targets. Version stays **0.4.4**.

**Field drivers (user, 2026-07-29):** the live Supertramp fixture
(`~/livetorrents/Supertramp – Even In The Quietest Moments...-1977`,
seven .wv files with the invalid APEv2 key `&год`, kept broken as the
standing acceptance fixture) plus the Wild Boys 32/384 conversion and
the user's standing DSP-honesty rulings. All mechanisms below were
traced to the line at 14c9b04 by three research passes plus a live
repro; do not re-derive them.

---

## §1 Metadata write authority for fallback-recovered sources
(FIELD-BLOCKING — reproduced twice, incl. by the user)

### 1.1 Mechanism (verified end-to-end, live repro)

Converting the fixture produces outputs carrying ONLY
ALBUM / "ALBUM ARTIST" (raw key) / ARTIST / ENCODER / ReplayGain —
no TITLE, DATE, GENRE, COMMENT, TRACKNUMBER — even though the
round-9 tolerant fallback populated ALL of them into TrackMetadata
(the rendered filename proves TITLE was known). Two stacked causes:

1. **ffmpeg's APE parser aborts at the invalid key.** The single-file
   encode uses `-map_metadata 0`; ffmpeg copies only the items that
   sort BEFORE `&год` (Album, Album artist, Artist) and drops
   everything after (Genre, COMMENT, DATE, TITLE). The surviving-tag
   fingerprint on the repro output matches exactly.
2. **The metadata stage never writes for SingleFile sources.**
   `source_needs_authoritative_metadata`
   (src/convert/pipeline/plan_bridge.rs:697-700) matches ONLY
   `SourceKind::CueImage | SacdIso | DvdVideo`. Plain SingleFile
   conversions trust ffmpeg's copy entirely; the recovered
   TrackMetadata feeds naming and logs but is never written to the
   output. (`orchestrator_metadata_stage_required` plan_bridge.rs
   :686-694 gates on satisfaction; the authority predicate is the
   root.)

### 1.2 Required

- The materializer marks recovery: when the tolerant APE fallback
  supplied the tag set (the seam that already pushes the
  "invalid APE key skipped" warning, materializer_single.rs:288-309),
  set a recovery marker. CARRIER (audit-ranked, pick one of TWO):
  (a) a reserved provenance extra key on TrackMetadata/AlbumMetadata
  extras (precedent: PRESERVED_SOURCE_ALBUM_TAG_EXTRA_KEY — safe
  against tag leakage because the extras-emission loop :4556-4561
  re-emits only source-provenance-prefixed keys), or (b) a
  `#[serde(default)]` bool on PreparedSource (it derives
  Serialize/Deserialize, types.rs:2026-2033). Do NOT derive the
  marker from the warning STRINGS (flattened at materializer_single
  .rs:294-300 — brittle). CONSTRAINT: the predicate signature is
  `(&PreparedSource)` and it also requires
  `prepared_source_has_metadata` (:699, helpers :702-738) — keep
  that conjunct; the marker must be readable from &PreparedSource or
  the signature and both callers change.
- `source_needs_authoritative_metadata` returns true for marked
  sources — the metadata stage then writes the authoritative tag set
  from TrackMetadata via the EXISTING writer
  (`authoritative_metadata_tags`, stages.rs:4452+ — already emits
  TITLE :4457, GENRE :4494, DATE :4499, TRACKNUMBER :4505, COMMENT
  :4514, canonical ALBUMARTIST :4473-4480, plus provenance-marked
  extras :4552+). No writer changes. AUDIT-CONFIRMED implementability:
  the obligations model ALREADY consumes the predicate
  (metadata_obligations_for_request plan_bridge.rs:587-600 sets
  authoritative_tags_applied from it at :597-598; the per-track
  planner hardcodes it false at :623 and can never satisfy it) — the
  predicate flip alone makes the stage run
  (planner_metadata_already_satisfied stages.rs:4004-4046 →
  orchestrator :24095-24108). The application machinery
  (apply_metadata :4222/:4232 → tag_audio_file :5206) is
  source-kind-agnostic — works for SingleFile artifacts as-is.
- Scope: fallback-recovered sources ONLY this round. Do NOT flip all
  SingleFile conversions to authoritative writes (blast radius;
  recorded as a possible future posture change).
- Bonus effect to pin: the authoritative write emits canonical
  `ALBUMARTIST`; ffmpeg's raw `ALBUM ARTIST` key no longer leaks
  into outputs for these sources (the extras carry the source key
  with provenance if preservation applies — state actual behavior in
  the report).
- Pins: fixture-shaped .wv (invalid key + full tag set) → the
  metadata stage RUNS (assert via the stage-decision seam /
  obligations, StubToolRunner acceptable per the existing
  conversion-pipeline test convention — a full real-tool encode is
  NOT required; assert authoritative_metadata_tags receives the
  recovered set and the plan requires the stage); a healthy .wv
  SingleFile conversion still takes the ffmpeg-copy path (stage
  skipped — pin the skip); one COMBINED §1×§4 pin: fallback-recovered
  + no track number + completion-order batch → ordinal filename, NO
  TRACKNUMBER tag emitted (stages.rs:4504-4510 gate).

### 1.3 Acceptance (user, live fixture)

Converting the untouched fixture yields outputs whose tags match the
source's valid items (canonical keys), with the invalid-key
disclosure in the log. The user will verify.

---

## §2 Dither override (ruling R1) — explicit selection beats the
32-bit gate

USER RULING (2026-07-29, recorded): the 32-bit no-dither DEFAULT
stands; an EXPLICIT user dither selection applies even at 32-bit int
targets ("that's on them"); every requested-but-not-applied dither is
disclosed. Also ruled: 32f→32i IS requantization.

### 2.1 Mechanism truths (all verified)

- TWO independent gate copies must change in lockstep:
  command construction — `target_depth_needs_dither`
  (tonepoet-pipeline/src/plugins.rs:1693-1695) and
  `pcm_conversion_reduces_depth` (:1699-1710) gating the four tool
  append sites (ffmpeg aresample :1329-1336 via `ffmpeg_needs_dither`
  :1321-1329; sox PCM :1546-1557; sox DSD→PCM :1681-1687 — the
  known silent drop; SSRC :358-410) — and the log gate
  `dither_applies` (stages.rs:16967-16990).
- The Foxy field case: `out_sample_fmt` is pushed unconditionally
  (:1338-1343) while `dither_method` is gated — hence undithered
  32f→32i requantization.
- Tool feasibility at Int32: **ffmpeg** — swresample dithers on the
  s32 output conversion; the existing `dither_method={method}` push
  works unchanged once the gate admits it (include the model's or
  applier's one-command empirical probe in the report). **sox** —
  trailing `dither` effect runs at output precision (both PCM and
  DSD→PCM builders). **SSRC — CANNOT** (no 32-bit dither stage;
  dither IDs rate-limited, mapping.rs:256-267): disclose
  "requested (…) — not applied (SSRC has no 32-bit dither stage)";
  do NOT add a post-SSRC stage (rigor-vs-usability directive).
  **Floats** — never dithered even when explicit; disclose.
  **Reference DSD** — untouched: Int32 stays fail-closed refused
  (dsd_reference.rs:3197-3209); Reference dither is policy-fixed and
  never consults settings.dither_type.
- **Gesemann ruling (audit-forced):** unmappable dithers are EXCLUDED
  from the relaxation arm. Today explicit Gesemann + Int32 + ffmpeg
  plans cleanly (the gate short-circuits before the
  `soxr_dither_method → None` planning error, plugins.rs:1330-1335);
  a naive relaxation would turn it into a NEW planning failure.
  Rule: the explicit-Int32 arm applies only when the tool can map the
  dither; otherwise disclose "requested (Gesemann) — not applied
  (not supported by the ffmpeg/soxr resampler)" and plan without
  dither. The existing planning error remains ONLY where dither
  genuinely plans today (needs_dither true).

### 2.2 Explicitness plumbing (the structural prerequisite)

- The bit EXISTS in the TUI and is DROPPED before the pipeline:
  `FormatState.dither_overridden` (app.rs:3404, doc :3402-3403; fn
  mark_dither_overridden app.rs:3866-3868; the user-rotation call is
  app.rs:4144 — reached via format_interactions.rs:13-15, NOT
  keybindings.rs — AND preset load presets.rs:632-640 sets it at
  :637; preset dither counts as explicit per the ruling; these are
  the ONLY three set-sites). `apply_auto_dither` (app.rs:4204-4247)
  machine-picks Shibata/TPDF WITHOUT setting it — **non-None value
  alone does not prove intent; do not build on it.** RESET SEMANTICS
  (fresh-eyes-found, RULED): a bit-depth rotation RESETS
  dither_overridden (app.rs:4190, immediately before auto-dither
  re-evaluates) — this is INTENDED and stays: explicitness is a
  property of the current selection state, so the deliberate gesture
  for 32-bit dither is picking the dither AFTER setting 32-bit
  depth. Do not "fix" the reset; pin it (explicit tpdf → rotate
  depth → flag false, auto None, default gate applies).
- Carry it into `PipelineSettings` (tonepoet-pipeline/src/settings.rs
  :22-65) as `dither_explicit: bool` with
  `#[cfg_attr(feature = "serde", serde(default))]` (per-field —
  the struct has no container-level default; precedent settings.rs
  :841/:1431) so old persisted queue JSON (queue.rs:206) and old
  rerun manifests (manifest.rs:36) deserialize. Populate INSIDE
  `format_state_to_pipeline_settings` (defined convert_actions.rs
  :292; the dither is mapped at :359 — add `dither_explicit:
  format_state.dither_overridden` there, covering all three callers:
  convert_actions.rs:137, :244, keybindings.rs:10819; the fn receives
  &FormatState so no signature change). Do NOT touch the legacy
  fallback_options path at :121 (no PipelineSettings there).
- **FINGERPRINT MANDATE (audit-forced — the biggest hazard the
  original draft missed):** the settings fingerprint hashes an
  EXPLICIT named-field inventory (fingerprint.rs:175-180,
  push_pipeline_settings :467-491), NOT serde fields — so the new
  field changes nothing automatically, and three outcomes are
  possible. REQUIRED: **mode-scoped emission** — emit
  `dither_explicit` into the fingerprint ONLY when it is
  output-affecting (i.e. `true`), which the inventory doctrine
  explicitly permits (fingerprint.rs:62-64). This keeps every
  existing fingerprint byte-identical (no mass manifest
  invalidation) while distinguishing the new behavior (no
  same-digest/different-commands hole in batch grouping
  processor.rs:417-418/:1103-1105 or rerun-manifest validation
  manifest.rs:591-627). FORBIDDEN: omitting it entirely (correctness
  hole — a stale undithered manifest would satisfy a dithered
  request) and unconditional emission (global invalidation +
  breaches the frozen-v1 doctrine fingerprint.rs:141-145). Update
  the sentinel inventory (tests/settings_sentinel.rs — the
  exhaustive struct literal :147-159 forces a compile-time touch;
  the inventory-coverage tests do NOT force the right choice — the
  mandate here does). CLI has no --dither
  flag today; if one is added later it is inherently explicit (out of
  scope now). DSD-target hardcoded None (convert_actions.rs ~:321)
  stays.
- Relax the gates to `needs_dither || (dither_explicit && target is
  Int32 && tool can map the dither)` at all four command sites AND
  `dither_applies` — floats excluded everywhere. SUBTLETY
  (audit-found): at the ffmpeg site `target_depth` can be None while
  `settings.target_bit_depth = Pcm(Int32)` — the Int32 arm must
  reuse the same Some/None→settings fallback shape as
  ffmpeg_needs_dither (:1321-1328) or the depth-carried-elsewhere
  path silently misses. The filter-emptiness check (:1344-1348)
  composes without change (explicit+Int32 already fails both
  emptiness conditions — verified). Extend the empirical probe
  requirement to sox as well (one command each; cheap).
- Pins: explicit tpdf + Int32 target → ffmpeg command contains
  `dither_method=triangular` alongside `out_sample_fmt=s32`; sox
  path gains the dither args; auto-selected (non-explicit) dither at
  Int32 still drops WITH disclosure; SSRC explicit case refuses with
  the disclosure; float target never dithers.

---

## §3 Unconditional DSP documentation (ruling R2)

USER REQUIREMENT (recorded): every conversion log affirmatively
documents each DSP stage — dither, resampling, bit-depth conversion —
yes/no + tool + algorithm + settings IN THE LINE, not only in the raw
command dump.

### 3.1 Seam (audit-grounded recommendation: settings-level
affirmative lines; per-track actuals stay in the existing
`Conversion:` lines)

- `append_conversion_settings_section` (stages.rs:16224-16292): the
  three omit-when-inapplicable gates (:16236-16253) become
  always-print affirmative lines:
  - `Dither: yes (TPDF via ffmpeg aresample)` /
    `no (not requested)` / `no (not needed — no bit-depth
    reduction)` / `requested (TPDF) — not applied (<reason>)` (the
    §2 disclosure rides here; reasons: 32-bit default gate [pre-§2
    only for non-explicit], float target, SSRC limitation, Gesemann
    planning constraint).
  - `Resampling: yes (soxr via ffmpeg aresample, 96000 → 44100,
    precision=28, cutoff=0.950)` / `no (source rate preserved)` —
    recompute effective parameters from the SAME mapping fns the
    command builder uses (mapping::soxr_precision / ffmpeg_cutoff,
    plugins.rs:1308-1314); tool from the executed commands via the
    existing `actual_resampler_label` precedent
    (stages.rs:17202-17231). Per-track heterogeneous rates already
    handled by `target_sample_rate_setting_label`'s BTreeSet
    labeling (:17233-17260).
  - `Bit-depth conversion: yes (24-bit → 16-bit, swresample
    quantization)` / `no (source depth preserved)`.
- Per-track: the existing `Conversion:` summary lines
  (`conversion_summary` stages.rs:16775-16863, transforms vec
  :16846-16858) gain the not-applied dither annotation for the §2
  disclosure case; the per-track warnings channel (:16025-16027) is
  the home for the per-track wording. Measured/verified depth honesty
  markers (:16821-16837) unchanged.
- Disposition derivation: recompute in stages.rs from
  settings + source depths (the same predicate shape both sides
  share) — do NOT thread new plan-level metadata (low-coupling
  option, stays consistent by construction).
- DSD Reference logs describe the policy-fixed dither
  ("yes (TPDF, sox_ng, Reference policy)" / "no (float terminal)").

### 3.2 Tests that pin the current conditional behavior (rewrite to
affirmative forms — all in mod conversion_log_tests,
stages.rs:39101-40038)

`resampler_settings_are_printed_only_for_actual_rate_changes`
(:39299 — asserts ABSENCE of "Resampler:", directly contradicted;
rewrite to assert the affirmative "no" form),
`soxr_settings_use_precise_labels...` (:39328),
`conversion_summary_shows_rate_depth_and_processing_changes`
(:39553), `dsd_source_rate_target_source_logs_planner_default...`
(:39598), DSD rate-label pins (:39674/:39696). The adjacent
conditional-SECTION pins (:39374, :39271) are unaffected unless the
section shape changes — don't change it. New pins per §3.1 line
form, incl. one asserting the full "requested — not applied" wording.

---

## §4 Untagged-track filename fallback (completion-order ordinal)

### 4.1 Mechanism truths

- Defect site: `track_number = metadata.track_number.unwrap_or(1)
  .max(1)` — materializer_single.rs:82 (TrackId :83-88). Untagged
  albums render "01 - Title.flac" for every track; when titles also
  collide, publish fails DestinationExists (stages.rs:20435-20436) —
  no cross-job suffixing exists (in-plan suffixing :2801-2822 is
  per-source).
- **The dispatch preview ALREADY uses the ordinal**:
  `prepared_source_from_dispatch_metadata` falls back
  `metadata.track_number.or(req.album_batch_track.track_number)`
  (stages.rs:2892-2897) — today preview and actual DIVERGE; this
  round closes it.
- Ordinal semantics: assigned from lexicographic path-sort + item
  index (processor.rs:1190-1200, normalized_path_key :1764-1768);
  correct for ≤9-per-side lettered names, lexicographic-not-natural
  for 10+ unpadded; stable within a run and for full re-runs;
  PARTIAL re-runs renumber. Disclosure must say "dispatch order",
  never "album order".
- Tags are safe by construction: the tag writer emits TRACKNUMBER
  only from `meta.track_number` / side-prefixed source values
  (stages.rs:4505-4511) — TrackId is never written as a tag.

### 4.2 Required

In `SingleFileMaterializer::materialize()` only: when
`metadata.track_number` is None AND `req.album_batch_track` is
present, use the batch ordinal for `TrackId.track_number` (mirror
stages.rs:2892-2897 exactly). Append to the existing completion-order
warning seam (materializer_single.rs:63-73): "filenames numbered by
dispatch order; no TRACKNUMBER tags written". DISCLOSED TRADE-OFFS
(audit-corrected — state in report): (1) COMPLETED old albums with
all-"01 - " names SKIP FOREVER (legacy rerun authority compares
settings fingerprint + source identity + recorded outputs,
rerun.rs:144-226/:390-464 — TrackIdentity is never compared outside
the DSD-Reference preflight :289; there is NO "mismatch once"); the
user renames or deletes old outputs to adopt new numbering.
(2) PARTIAL old runs: missing-manifest redo forces ReplaceWithBackup
(rerun.rs:374-388) so NO DestinationExists hazard at colliding names
— but old non-colliding files (old "01 - B.flac" vs new
"02 - B.flac") are NOT removed → duplicate audio under two
numberings persists; disclose. (3) Partial re-runs renumber;
(4) lexicographic-order caveat. Collision machinery untouched.
DOC CONTRACTS (audit-forced): the change makes two comments false —
processor.rs:1215-1216 ("coordination identities only … prevents
them from being rendered as track order") and the
AlbumBatchTrackContext.track_number field doc (types.rs:326-329).
REWRITE BOTH to state the new contract (ordinals may render as
FILENAME numbers for untagged completion-order tracks; never as
tags, never as log ordering).
Pins: untagged 3-file completion-order batch renders 01/02/03
filenames with the disclosure and NO TRACKNUMBER tags on outputs;
tagged tracks unaffected; preview == actual for the fallback case.

---

## §5 Padded numbering for the APE family

### 5.1 Mechanism truths

- Capability lattice: `numbering_capabilities`
  (metadata_persistence.rs:694-708); NativeWavPackApe AND LoftyApe =
  PLAIN_UNSIGNED_ONLY; flags plain_unsigned/numeric_fraction/lexical
  (:637-645). Classification: bare "01" = Lexical (leading zeros
  rejected by is_canonical_positive_unsigned :1090-1094); "01/12" =
  NumericFraction (:1101-1111). Scheme mapping: NN → Lexical today
  (metadata_autonumber.rs:487-501) — hence the N-only menu on the
  fixture (numbering_menu_eligibility :668-696).
- Padding SURVIVES both write paths verbatim (native writer composes
  raw strings, probe.rs:8663-8737; healthy files go through lofty
  which serializes item STRINGS verbatim — tonepoet writes via
  insert_unchecked, probe.rs:9882-9928) and both read paths (no
  normalization anywhere) — the editor can already DISPLAY "01" it
  is forbidden to write. No spec/compat reason to refuse (APEv2 is
  free text; the PLAIN_UNSIGNED_ONLY stance was round-trip honesty
  vs lofty's typed accessors, not an APE constraint).
- **Trap (must fix together):** the lofty idempotency preflight
  compares via typed accessors that normalize "01"→"1"
  (typed_numbering_accessor_value → tag.track()); a padded value
  re-enters the full write transaction on every repeat write,
  breaking assert_lofty_repetition_skips_transaction
  (probe.rs:12795-12799). Fix the preflight to raw-text (or
  numeric-equivalent + raw) comparison as part of this item.
- **Both backends must widen**: healthy-WV writes validate at route
  dispatch (NativeWavPackApe, probe.rs:9481-9486) AND inside the
  lofty prepare (LoftyApe from tag type, :9974-10003).

### 5.2 Required

Add `MetadataNumberingRepresentation::PaddedUnsigned` (pure digits,
leading zero, parses > 0) + capability flag; classify bare "01" as
PaddedUnsigned (padded fractions stay NumericFraction); map
`NumberingScheme::NN` to PaddedUnsigned; grant plain + padded +
fraction (lexical stays false) to NativeWavPackApe AND LoftyApe.
**TEXTUAL CORRECTION (audit-forced — the original wording regressed
FLAC/Vorbis):** NN is TODAY accepted on TEXTUAL backends via
lexical=true; after the remap, TEXTUAL MUST gain padded=true (or
supports() must treat lexical as a superset of padded — pick one and
state it) so FLAC/Vorbis keep NN and keep accepting "01" — the
existing pin shared_mutation_engine_updates_values_and_dirty_state
(metadata_autonumber.rs:1757-1780, applies NN to .flac files) must
stay green. DSF/ID3v2/MP4 explicitly keep padded=false (ID3v2 TRCK
padding is a SEPARATE undecided question — fenced).
**PREFLIGHT FIX (audit-forced — both naive shapes fail):** the
idempotency preflight is shared by LoftyId3v2/LoftyApe/LoftyMp4Ilst
(probe.rs:10007-10017; Vorbis takes a different path :10006).
Raw-text-only compare breaks MP4 (trkn/disk are binary atoms — every
repeat write re-enters the transaction, breaking
mp4_numbering_pairs_round_trip_without_free_form_atoms
probe.rs:13533-13537); numeric-equivalent-OR-raw silently NEVER
writes the padding on APE ("01" numerically equals "1" → wrongly
satisfied). REQUIRED: capability-aware compare — raw-text where the
backend's capability includes padded (the APE family), typed
accessor compare elsewhere (MP4/ID3v2 unchanged). Menu result on APE
files: N, NN, N/NN, NN/NN; Custom/SN/SNN still refused.
Pins to flip/extend (enumerated by research):
`ape_numbering_capability_matches_production_round_trip`
(probe.rs:13284-13291; the rejection loop :12844-12871 — "01",
"7/17", "01/17" flip to accepted; "A01" stays rejected);
`numbering_representation_classification_is_lossless_and_explicit`
(metadata_persistence.rs:1762-1782 — "01" reclassifies);
capability-const exhaustive-match ripples (:647-680); DSF/ID3v2/MP4
pins stay green only if their flags stay false — verify; repetition-
skip pin gains a padded case. ADDITIONAL RIPPLES (audit-enumerated):
the shared rejection helper's message expectation
(error.contains("unsigned"), probe.rs:12855-12858) depends on the
capability being exactly PLAIN_UNSIGNED_ONLY selecting the
"canonical positive unsigned" hint (metadata_persistence.rs
:1146-1152) — split expectations between APE call sites (new
message) and ID3v2 (:13276-13282, unchanged); the helper's
capability-equality assert (probe.rs:12770-12772) parameterizes for
APE; the scheme-rejection wording test
(metadata_autonumber.rs:1663-1680) gains a padded arm;
requirement-label matches (metadata_persistence.rs:1141-1145,
metadata_autonumber.rs:545-555) are compiler-forced ripples.

---

## §6 Ledger items (small, mechanical — from rounds 8-9 audits)

1. Queue LIST row shows the warning count ("Completed (2 warnings)")
   — today only the detail surface shows it (draw_queue.rs:358-364
   vs draw_overlays.rs:1539-1553).
2. Legacy-JSON pin: a persisted `Completed` WITHOUT `warning_count`
   deserializes (queue.rs legacy-deserialization pin family
   :1519-1548 is the pattern).
3. ComponentSanitizer parity pin strengthening (audit-specified
   shape): drive the actions.rs rename-planner unit (:7054-7074)
   with the PRODUCTION semantics (action_semantics(),
   stages.rs:20852-20860) and a Template action, asserting dot-run
   retention through the fn-pointer INVOCATION SITE — unit-level, no
   filesystem pipeline required (the current pin at
   stages.rs:41477-41483 is wrapper==wrappee equality).
4. The five round-9 hermeticity guards in src/tui/probe.rs gain the
   per-test rationale comment (match tag_interchange.rs:928-930's
   wording). The five, BY NAME (do not comment-blast the ~60 other
   guard sites): native_wavpack_write_preserves_invalid_ape_item_
   byte_exactly, native_wavpack_empty_string_deletes_ordinary_ape_
   item, native_wavpack_empty_numbering_deletes_or_reduces_combined_
   item, native_wavpack_write_is_byte_idempotent_for_matching_read_
   only_item, inline_wavpack_dispatch_bypasses_legacy_database_and_
   selects_serializer.

## §7 Fences (unchanged this round)

Pairing-guard exact-sequence relaxation (STILL awaiting the user's
round-8 multi-FILE transfer field evidence); vinyl side-number
PARSING (collision fix shape (a)); flipping ALL SingleFile
conversions to authoritative metadata writes (§1 records the
posture question); ID3v2/MP4 padded numbering; CLI --dither flag;
post-SSRC dither stages; reserved-char naming table; `…`
substitution; mount-capability naming; external clipboard tools;
scan-at-scale repair tooling; Custom builder + Paste tags (user has
mockups — STILL QUEUED, keeps sliding, next feature round); config
cascade; library. NO F-keys; NO emoji/decorative unicode; Ctrl+Q
stays quit; version 0.4.4; never truncate gate output; rounds 5-9
machinery must not regress (round-9 pins stay green; the §2/§3 gate
relaxations must keep the DSD Reference qualification suite green).

## §8 Deliverables

Overlay bundle (tar.gz, nested dir, preimage manifest with SHA-256 of
exact base revisions) + engineering report with: per-item named pins
(minimum: §1, §2, §3.2 rewrites + new affirmative pins, §4, §5 flip
list, §6), the §2 empirical ffmpeg AND sox s32-dither probe results, the §1
authoritative-write scope statement, disclosed limitations (§4
trade-offs; SSRC/float dither refusals; lexicographic ordinal
caveat), the §2 fingerprint mode-scoped-emission statement, the §5
TEXTUAL-padded resolution and preflight per-backend compare rule
stated explicitly, and any deviation with rationale.
`cargo test --workspace` green against 5,354/0; new tests must FAIL
if the behavior they pin regresses.
