# Implementation report — submitted-batch DSD→PCM album auto-gain

Date: 2026-08-27
Baseline requested by work order: `main` @ `3d3f1e1`

## Result

Implemented an opt-in `DsdAutoGainScope::{Track, Album}`. `Track` remains the default and preserves the historical one-input SoX `norm` path. `Album` derives one fixed gain from the loudest reconstructed DSD track in the exact user-submitted batch and applies that same gain to every participating DSD track. Non-DSD members of a mixed submission do not receive the gain.

The implementation deliberately supports both automatic regimes:

- live legacy `DsdToPcmGainMode::Auto`;
- native-v2 `DsdSourceGainMode::NormalizePeak`.

Native album normalization remains outside the qualified Reference contract. No file under `assets/dsd_reference/` is modified.

## Corrective review pass — 2026-08-27

A subsequent adversarial review found three genuine defects in the first implementation. This bundle corrects all three without changing the retained-PCM/barrier architecture.

### C1 — native Album reconstruction profile is now manifest-v1 byte authority

Native `NormalizePeak + Album` remains deliberately outside the qualified Reference manifest path, so its rerun identity still comes from the legacy manifest-v1 settings fingerprint. The first implementation included album scope and resolved gain there, but omitted the native reconstruction selector consumed by `build_album_gain_analysis_command()`. Reference and Wideband reconstruction could therefore produce different retained PCM under an identical manifest settings fingerprint whenever their measured peaks quantized to the same SoX centidecibel value.

The correction adds exactly one conditional v1 extension field, `dsd.album_native_from_dsd_profile`, and emits it only when both `is_native_v2()` and album automatic gain are active. No native qualification/toolchain fields are imported into legacy manifest identity. Track scope remains outside the extension entirely, and legacy Album mode does not acquire inert native profile state.

Focused source regressions cover identical bound album gain with Reference versus Wideband, distinct legacy-manifest fingerprints for those profiles, native Track compatibility, legacy Album profile inertness, and the existing frozen v1 sentinel remains unchanged.

### C2 — `SkipIfManifestMatch` is re-evaluated after the gain barrier

The pre-measurement rerun gate remains suppressed for album mode because runtime album gain does not exist yet. After the complete submitted batch is measured and the shared gain is bound, `SkipIfManifestMatch` albums now enter a small scheduler work unit before track fan-out. That worker runs the existing `decide_rerun()` manifest/source/output equivalence machinery with the now-complete settings.

A true match completes through the existing `finalize_manifest_skip()` semantics and is never registered for track fan-out, so no final encode or replacement publication is performed. Mismatch, corrupt/missing manifest, changed source facts, changed aggregate gain, or changed native profile returns the album to the unchanged encoder path. `VerifyIfManifestMatch` is intentionally untouched.

Manifest/source/output I/O and hashing do not run synchronously in the central scheduler loop. The scheduler only queues the resolved album for worker-side rerun evaluation and handles the resulting terminal skip or normal release.

Pre-action semantics are preserved: if durable pre-actions have already executed before this deferred decision, a later manifest match logs the established warning and proceeds rather than retroactively representing the run as wholly skipped.

### C3 — durable conversion records now disclose exact gain-scope membership

At the completed submitted-batch barrier, the implementation builds one deterministic runtime-only disclosure in persisted submission order and source-track order. Each participating DSD entry contains its original source path plus exact `TrackId` (`source_ordinal`, disc number when present, and track number). The disclosure also records submitted item count, excluded non-DSD track count, and the number of submitted items containing no participating DSD tracks.

The same disclosure is copied to every scheduled member solely for reporting. Both standalone conversion logs and fragment-backed independent-file album logs emit an `Album gain scope` line next to the track record, so A+B and A+C are distinguishable even when cardinality is identical; multi-track ISO/CUE/archive members are distinguishable by track identity rather than image pathname alone; and mixed DSD/PCM submissions state what was excluded.

The disclosure is not serialized into settings and does not affect settings fingerprints or encoded audio. Cohort membership continues to affect output identity only through the resolved aggregate gain.

## Decision 1 — measurement point and decode cost

### Choice

Measure while producing a retained post-reconstruction Float64 CAF carrier, before the submitted-batch barrier. The same SoX invocation both writes the carrier and runs the non-mutating `stats` effect. After all submitted DSD tracks report their peaks, the scheduler resolves one fixed gain and releases the retained PCM carriers to the ordinary final encoder.

This means the expensive DSD reconstruction/decode happens **once per track**, not twice:

1. DSD input → final-rate Float64 CAF + `stats` (one DSD reconstruction pass).
2. Barrier resolves the shared gain.
3. Float64 CAF → requested final format with the fixed gain (PCM read/encode only).

The carrier is an explicit `TrackSourceRef::DsdAlbumGainCarrier`. It is audio-only: original DSD identity remains separately attached for metadata, companion-file, template, provenance, and conversion-log policy. Planner source tag/artwork/source-MD5 transfer is disabled for the carrier so the temporary CAF cannot become metadata authority.

### Why CAF

CAF avoids the RIFF/WAV 4 GiB ceiling and does not inherit this repository's documented Float64 W64 interoperability problem between its FFmpeg/SoX paths. SoX 14.4.2 in this runner successfully wrote and reread 64-bit floating-point CAF.

### Empirical transparency check

A control Float64 CAF and a CAF produced by the same SoX conversion with a trailing `stats` effect were decoded to raw Float64 PCM. Their SHA-256 digests were identical. Thus, in the exercised SoX 14.4.2 path, adding `stats` did not alter the retained samples.

### Cost

The design trades a second DSD decode for retained PCM I/O/storage. For stereo Float64 carriers, payload cost is approximately:

- 88.2 kHz: 5.080 GB/hour (4.731 GiB/hour)
- 176.4 kHz: 10.161 GB/hour (9.463 GiB/hour)
- 352.8 kHz: 20.321 GB/hour (18.926 GiB/hour)

Long/multi-track sources whose retained size cannot be bounded safely before materialization bypass memory-backed scratch and use disk staging. Standalone DSD scratch admission adds a bounded carrier reservation. A track reported as `-inf` temporarily needs one additional carrier-sized raw Float64 stream for the independent signed-zero proof; that is an extra PCM read, not an extra DSD reconstruction.

## Decision 2 — attenuation and headroom policy

Album mode allows both boost and attenuation. The fixed gain is derived as:

`gain = target_dbfs - conservative_loudest_peak_dbfs`

Therefore a loudest track already above the requested target attenuates the entire DSD set uniformly. There is no clamp and no refusal merely because the correction is negative.

SoX 14.4.x prints `Pk lev dB` at 0.01 dB resolution. Because album mode must bind an explicit fixed gain from the text report rather than letting `norm` retain SoX's internal peak, the implementation reserves one complete 0.01 dB reporting quantum before deriving gain. This local reserve prevents a downward-rounded printed peak from causing a small target overshoot. It is **not** imported Reference qualification policy.

An all-silent DSD scope receives exactly 0 dB gain after independent signed-zero proof; there is no finite peak to normalize.

Empirical two-track check in this runner:

- reported input peaks: −17.99 and −5.99 dBFS;
- target: −0.15 dBFS;
- shared gain: +5.83 dB;
- reported output peaks: −12.16 and −0.16 dBFS;
- inter-track difference before/after: 12.00 dB / 12.00 dB.

## Decision 3 — degradation policy

Measurement is fail-closed at submitted-batch scope.

If any participating DSD track cannot be realized, decoded, measured, parsed, or (for `-inf`) independently proved silent, the submitted batch does **not** silently fall back to track normalization and does **not** exclude the failed track. Ready siblings held at the barrier are refused and surfaced as failed. This avoids applying an authority derived from a set smaller than the one the user submitted.

Persisted queue items carry a random opaque `submission_id` plus declared `submission_size`. On retry/resume, an incomplete cohort is refused rather than silently changing album scope. Scope is never inferred from folder structure, tags, album identity, or disc layout.

A mixed DSD/non-DSD submission is allowed. Only DSD tracks participate in measurement and receive the shared gain; non-DSD tracks remain ordinary conversions. A submission containing no DSD tracks passes through with no album authority.

WavPack hybrid output is refused for album mode because the native hybrid encoder path has no place to apply the submitted-batch fixed gain. Ordinary WavPack remains available through the normal processing encoder path.

After a complete authority has been bound, ordinary per-track conversion/publication failure semantics remain in force. The measurement barrier does not redefine the product's existing partial/fail-fast policy after authority formation.

## Decision 4 — gain regimes

Album scope is an orthogonal setting attached to both automatic regimes:

- legacy `Auto + Album` now;
- native `NormalizePeak + Album` after/when native controls are used.

`Track` keeps the existing behavior and serialization by default. Native `NormalizePeak + Album` explicitly bypasses qualified Reference planning and uses only reconstruction mechanics needed to create the retained PCM carrier. The frozen Reference corpus and qualification policy remain untouched.

For native submitted-batch homogeneity, album preflight uses the native-v2 settings snapshot fingerprint rather than the frozen manifest-v1 compatibility fingerprint. The latter intentionally omits native directional DSD fields and therefore cannot prove equal reconstruction profiles. Legacy submissions continue to use the legacy v1 settings key.

## Decision 5 — exposure

The option is surfaced through the existing settings paths:

- CLI: `--dsd-auto-gain-scope track|album`;
- TUI format pane: `gain scope` pill;
- command mode: `:set dsd-gain-scope track|album`;
- presets: optional `dsd_auto_gain_scope`.

Preset compatibility is intentionally asymmetric: Track is omitted from serialized presets, so old/default presets keep their prior wire shape; only the opt-in Album value is written.

CLI `--dsd-gain auto` continues to mean the live legacy auto-normalizer. Native `--dsd-gain normalize` can independently select album scope. A scope-only CLI override is accepted when the loaded preset already selects one of those automatic modes.

## Idempotency and visibility

Track/default fingerprints remain unchanged. Album mode adds fingerprint fields only when the album automatic mode is active:

- configured album scope;
- resolved fixed album gain once the barrier binds it;
- for native-v2 Album only, the Reference/Wideband reconstruction profile actually consumed by the unqualified reconstruction command.

The ordinary manifest-v1 fingerprint therefore identifies the byte-affecting authority for both legacy Auto and native NormalizePeak while leaving every Track fingerprint unchanged. Native-v2 snapshots continue to identify the wider native reconstruction settings for scheduler homogeneity. A different submitted set may legitimately resolve a different gain; `SkipIfManifestMatch` is suppressed before measurement and re-evaluated on a worker after the aggregate is recomputed and bound.

Successful conversion summaries record the shared gain, target, analyzer reserve, measured DSD track count, and loudest reported peak (or verified-silence status). Durable conversion records additionally contain the deterministic exact DSD participant list and explicit non-DSD exclusions for the submitted cohort.

## Coverage added

Source tests cover, among other cases:

- loudest track drives aggregate gain;
- one gain preserves the inter-track level difference;
- attenuation when the loudest peak exceeds target;
- 0.01 dB reporting reserve/headroom property;
- finite + silent tracks and all-silent unity;
- strict SoX peak parsing;
- empty aggregate refusal;
- carrier planning requires DSD semantic authority and final-rate PCM;
- gain emitted only on the processing encode step (SoX and FFmpeg);
- complete/incomplete submitted-batch preflight;
- early terminal participant cannot leave the barrier hanging;
- native-v2 reconstruction-profile heterogeneity is refused;
- track/default fingerprint compatibility;
- legacy and native album scope/resolved gain affect the execution fingerprint;
- runtime album authority is never serialized;
- native Reference/Wideband Album profiles differ in manifest-v1 identity even with identical bound gain;
- legacy Album does not acquire inert native profile state, and Track/default compatibility remains frozen;
- post-barrier matching `SkipIfManifestMatch` terminates through manifest-skip semantics before fan-out;
- changed aggregate gain, source facts, native profile, or already-executed pre-actions do not incorrectly skip;
- equal-cardinality A+B versus A+C cohorts produce different durable scope records;
- multi-track source disclosures retain source ordinals/disc/track identity and mixed submissions report non-DSD exclusions;
- scope disclosure alone does not change the settings fingerprint;
- Browse multi-path admission preserves one submitted-batch identity and persisted submission order;
- unbounded multi-track album carriers bypass memory-backed scratch.

## Findings that changed the brief's suggested shape

The work order correctly points at the existing independent-single-file album-batch preparation as a nearby cross-item boundary, but that grouping is folder/output-policy derived. The required scope is the exact user submission, so reusing that identity as album-gain membership would be incorrect. The implementation instead adds an opaque submission identity at queue admission and a dedicated scheduler barrier. Existing folder/tag-derived album batching is left intact for its original purpose.

The native-v2 `ResolvedGainPolicy::FixedExact` vocabulary is useful prior art, but the retained carrier intentionally enters the ordinary planner as PCM. Reusing the qualified Reference deferred-gain machinery would drag this unqualified feature into Reference attestation/ceiling obligations. The fixed album gain is therefore a runtime-only DSD authority consumed by the ordinary PCM encode builders.

## Verification status

**The mandatory Rust/Nix gate was not executable in this runner.** The environment contains neither `nix`, `cargo`, `rustc`, nor `rustfmt`; `/nix/store` is absent, and package-network access is unavailable. I therefore do **not** claim that `cargo test --workspace --no-fail-fast` passed, do not claim the requested double run, and cannot certify “no new compiler warnings” from this environment.

Best-effort verification completed here:

- `git diff --check`: clean;
- delimiter/comment/string-aware structural scan across all corrective Rust edits: clean;
- corrective call-site/ownership audit for every extended conversion-log function and every `ScheduledAlbum` constructor: clean;
- conflict/TODO marker scan of added code: clean;
- `assets/dsd_reference/`: no changes;
- exhaustive static audit of `TrackSourceRef::DsdAlbumGainCarrier` match/use sites;
- SoX 14.4.2 Float64 CAF write/read smoke test: passed;
- SoX same-pass `stats` sample-transparency SHA-256 check: passed;
- two-track shared-gain/headroom/ratio semantics test: passed.

The required certification gate remains, exactly as requested by the work order:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

Every `test result:` line must be `0 failed` before this bundle should be called certified for hand-off.

## Corrective review pass 2 — terminal typing and scope retention

A second review found two narrow defects in the corrected tree. Both are fixed in this R2 bundle without changing album-gain settings, fingerprinting, the submitted-batch barrier, or the retained Float64 CAF design.

### C4 — album-analysis failure now passes no nonexistent artifacts

The failure branch after `prepare_album_gain_carriers()` previously passed `Some(&album_plan)` to `publish_terminal_conversion_log_fragment_if_needed()`, whose third argument is `Option<&ArtifactSet>`. `AlbumPlan` and `ArtifactSet` are unrelated types, so the tree could not type-check.

That argument is now `None`. This is the semantically correct state: analysis failed before final conversion artifacts exist. The terminal helper already seeds an empty artifact set when `None` is supplied, while `album_plan` remains available separately to `finalize_report()`.

No terminal-helper type was widened and no synthetic artifact set is constructed from planning state.

### C5 — post-authority terminal logs retain exact submitted gain scope

The resolved `DsdAlbumGainScopeDisclosure` was previously passed only through the normal Features path. Merge/metadata/ReplayGain/Features failures, blocked outcomes, and cancellation inside `finish_pipeline_album_for_scheduler_with_tool_limits()` fell back to the older terminal helpers, which staged conversion-log sidecars with a hard-coded `None` disclosure.

The existing scope-blind helpers are preserved for pre-authority and unrelated callers. Two narrow internal variants now accept `Option<&DsdAlbumGainScopeDisclosure>` and pass it to the already-existing `stage_conversion_log_sidecars()` parameter. Every terminal and cancellation exit inside the post-barrier scheduler finisher uses those variants with `album_gain_scope_disclosure.as_ref()`.

Pre-barrier materialization/planning/analysis failures still use the original helper and therefore cannot fabricate a resolved cohort. The disclosure remains runtime/reporting state only; settings, fingerprints, manifests, and encoded audio identity are unchanged.

A focused async regression drives the real post-barrier finisher with successful final conversion artifacts, a bound two-track album gain, exact A+B disclosure, and an injected ReplayGain failure. The terminal `conversion.log` is required to retain both A and B identities plus the existing submitted-batch gain/count summary. An existing terminal-failure test now also asserts that a path without resolved authority contains no `Album gain scope` line.

### R2 verification limitation

The mandatory build gate still cannot be executed in this runner: `nix`, `cargo`, `rustc`, and `rustfmt` are absent. This R2 bundle therefore remains **UNCERTIFIED**.

R2-specific static verification completed here:

- the invalid `Some(&album_plan)` terminal-helper argument is absent;
- every terminal/cancellation exit in the post-barrier finisher uses the scope-aware helper;
- pre-barrier callers continue to use the scope-blind helper;
- `git diff --no-index --check` against the preceding corrected bundle is clean;
- the modified Rust file passes the delimiter/comment/string-aware structural scan;
- no qualified Reference asset was changed.

The required Nix workspace gate must still run twice, with every `test result:` line reporting `0 failed`, before certification.

## Corrective review pass 3 — scratch retry preserves submitted-batch authority

A third review found one ordinary recovery-path defect: after the submitted-batch barrier had resolved a fixed album gain, either a final-track scratch ENOSPC or a post-conversion scratch ENOSPC could still hand the single item to the generic serial disk retry. That legacy retry rematerializes the original DSD source, and the planner correctly strips runtime album authority from non-carrier inputs; live legacy Auto would then emit track-scoped `norm -<headroom>` for the retried item. The retry could therefore complete successfully with audio that violated the selected Album scope.

### C6 — resolved album gain has a dedicated disk retry

The generic scratch retry remains unchanged for ordinary Track mode. Resolved Album authority is now intercepted at the existing `run_album_postprocess_work_scoped()` / scheduler-finisher boundary:

- the pre-finisher track-output ENOSPC branch does not invoke the generic serial retry when `runtime_album_gain_db` is bound;
- the scheduler finisher captures the existing carrier-based prepared source, its scratch staging root, fixed runtime gain, exact `DsdAlbumGainScopeDisclosure`, plan/stage state, and pre-action output capability before destructuring the scheduled album;
- on a recognized scratch-scoped failure it creates ordinary disk staging for the retry outputs but reuses the already-measured Float64 CAF directly from the still-live scratch staging tree;
- the outer finisher continues to own the scratch `StagingDir` for the entire awaited retry, so that retained carrier cannot be cleaned up until retry encoding/post-processing has completed;
- no retry measurement is run at all. The already-resolved submitted-batch gain is checked for exact equality before final encoding and is never passed back through `resolve_album_gain()`;
- final encoding is re-run from the same `DsdAlbumGainCarrier`, so the existing carrier-only planner guard retains the fixed gain and continues to prevent non-participating PCM inputs from inheriting DSD authority;
- the original exact submitted-batch disclosure is copied into the retry album and therefore survives either successful or terminal retry logging;
- if the album-aware retry cannot be prepared or completed, the original attempt finishes visibly as a terminal failure rather than falling through to the generic source-from-scratch retry;
- a defensive processor guard also refuses any resolved-album report that somehow reaches the old generic postprocess retry intent.

This exceptional recovery does **not** reconstruct DSD a second time and does not copy the potentially multi-gigabyte Float64 carrier to disk. Only retry outputs move to disk. The carrier is reused read-only while its original scratch owner remains alive. Recomputing the complete submitted cohort or introducing an aggregate cache was intentionally avoided.

Scratch admission already bypasses multi-track Album sources whose retained-carrier bound is not known up front. The dedicated retry therefore accepts the independently submitted `SingleFile` shape that can actually reach this scratch path and fails closed if that invariant changes, rather than silently treating an ISO/CUE/archive as one retried track.

### R3 focused coverage

New focused coverage includes:

- scheduler-level final-track scratch ENOSPC with two submitted DSD participants: the album-aware retry sees the original A+B disclosure and the already-bound `+2.840000000 dB` authority, while the generic serial retry hook is forbidden;
- scheduler-level post-conversion ReplayGain scratch ENOSPC exercises the same album-aware retry and the same A+B authority/disclosure invariant;
- a planner regression constructs the retained carrier used by the retry and requires the final command to contain fixed `gain 2.840000000` and no legacy `norm` argument.

The plan-bridge carrier-only containment rule is unchanged. Non-carrier PCM/non-participating inputs still have runtime DSD gain stripped, and ordinary Track-mode scratch retry behavior is unchanged.

### R3 verification limitation

The mandatory build/test gate still cannot be executed in this runner because `nix`, `cargo`, `rustc`, and `rustfmt` remain absent. This R3 tree therefore remains **UNCERTIFIED** pending the required two Nix workspace runs.

R3-specific verification completed here:

- `git diff --no-index --check` against the exact R2 bundle is clean;
- delimiter/comment/string-aware structural scans pass for `stages.rs`, `processor.rs`, and `plan_bridge.rs`;
- static call-site audit confirms bound runtime album gain is excluded from both generic serial scratch-retry branches;
- the dedicated retry contains no call to `resolve_album_gain()` or `prepare_album_gain_carriers()` and retains the exact fixed gain while reusing the original measured carrier;
- the post-retry planned carrier path is covered by a fixed-gain/no-`norm` command regression;
- no qualified Reference asset or policy file is changed.
