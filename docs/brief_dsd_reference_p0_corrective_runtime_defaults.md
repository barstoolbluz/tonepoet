# Corrective Brief: Pre-Promotion Runtime Defaults, SACD Plan Probing, Post-Final Ceiling

**Status:** your analyzer-carrier corrective v5 is applied at `b85cce1` and
compiled. The streamed two-process analyzer pipeline is sound and the
tool-gated transport/defect-reproduction proofs pass (2/3 gates). Three
defects remain, all reproduced with evidence below. Resolve all three in
one round. The tree is deliberately unshipped until this lands.

Already fixed on the apply side (do not redo; review the diffs in
`b85cce1` and keep them): three environment probes asserted `path=unset`,
but a shell exec'd with a cleared environment self-assigns libc
`_PATH_DEFPATH`, so the probes now assert HOME/ambient-PATH absence plus
the LC_ALL allowlist (your `env_clear` implementation is correct); the
settings-sentinel policy pin was stale at v3; the manifest
fingerprint-mutation test helper now mutates route AND per-track
identities to satisfy your (correct) cross-validation.

## D1 — BLOCKING production regression: default settings break every DSD→PCM conversion

Reproduced live: `tonepoet convert test_dsd64.dsf --format flac` fails:

```text
DSD-REF-P0-015: The installed Reference toolchain does not match policy
sox_ng_14_8_0_1_v4 or failed its behavior probes. … (the embedded policy
artifact is not a qualified v4 release)
Conversion complete: 0/1 succeeded, 1 failed
```

Chain: `DsdSettings::default()` is native-v2 origin (TUI builds it at
`src/tui/convert_actions.rs:381`; CLI likewise) → `plan_conversion_with_registry`
routes native-v2 + DSD source + PCM target **exclusively** to
`plan_reference_dsd` with no legacy fallback
(`tonepoet-pipeline/src/plan.rs:759-764`) → the runtime attestation gate
correctly refuses the unpromoted `qualification_candidate`. Net effect:
"runtime exposure disabled" became "all default-settings DSD→PCM
conversions disabled." The design brief's §8.2 default flip to Reference
was shipped before promotion; the commission's driving requirement was a
DSD→PCM path that works today.

Directive — restore working conversions pre-promotion while keeping the
fail-closed Reference posture. Choose and implement one shape, with
rationale:

(a) `DsdSettings::default()` remains legacy-equivalent until a promoted
    policy exists; native-v2 is explicit opt-in (and becomes the default
    in the promotion release). Update the sentinel/serde/fingerprint pins
    that currently assert a native-v2 default.
(b) Keep native-v2 defaults, but planning falls back to the documented
    legacy chain whenever the embedded policy is not a qualified release
    — with an explicit, logged "Reference unavailable (candidate policy);
    using legacy conversion chain" provenance line, and a hard flip to
    Reference-only at promotion. The fallback must emit the exact legacy
    argv (your `LegacyFlatV1` compatibility view) and legacy manifest
    route identity, never a half-native hybrid.

Whichever you choose: the live smoke (`DSD64 DSF → FLAC` with default
settings, real tools) must succeed, and it becomes a permanent tool-gated
test so this class cannot ship silently again.

## D2 — SACD plan-time TOC probing breaks four pre-existing tests (same root)

`plan_request_for_track` now probes Reference source kind for every
native-v2 + DSD-source plan (`src/convert/pipeline/plan_bridge.rs:122-124`);
for `TrackSourceRef::SacdTrack` that reads the SACD TOC at plan-build
time. Four pre-existing tests assert **legacy** SACD planning properties
against synthetic `album.iso` paths that do not exist and now fail with
`failed to read SACD TOC for Reference: … No such file or directory`:

```text
plan_bridge::tests::sacd_flac_plan_has_no_ffmpeg_map_metadata_or_source_md5_from_materialized_dsf
plan_bridge::tests::sacd_plan_request_suppresses_unsupported_source_tag_artwork_md5_policy
plan_bridge::tests::staged_dsf_and_dff_flac_plans_still_use_ffmpeg_source_metadata_transfer
track_executor::tests::sacd_track_plan_reports_no_planner_metadata_satisfaction_for_source_tag_policy
```

Resolve consistently with your D1 choice: under (a) these tests keep
default (legacy) settings and must not probe; under (b) the probe should
not run — or must not hard-fail the plan — when the plan will take the
legacy fallback anyway (SACD Reference cells are deliberately unavailable
in P0, so a plan-time TOC read that exists only to reject afterward is
wasted I/O and a new failure surface). Also state the intended policy:
should Reference source-kind probing ever perform SACD TOC I/O at
plan-bridge time, or belong wholly to executor preflight where the
double-SHA identity checks already live?

## D3 — qualification gate: real chain exceeds the −1 dBTP post-final ceiling

`complete_p0_reference_qualification_report` fails at
`tests/dsd_reference_qualification.rs:2116` with the production error:

```text
invalid settings for dsd.reference.post_final_true_peak:
post-final true peak exceeds the Reference -1.000000000 dBTP ceiling
```

The analyzer carrier now measures correctly (that was the point of v5),
and the honest post-final measurement rejects your own chain's output.
This is the acceptance gate doing its job against the gain authority: the
pre-final `TP_upper`-based `maximum_ceiling_safe_gain`, the terminal
realization bound ε/S, or the fixture's requested gain resolution is not
conservative enough for the real toolchain — or the fixture legitimately
requires a constrained gain the test then mis-asserts. Diagnose with the
real numbers (the log records reported/conservative pre- and post-final
peaks, ε, S, requested/applied gain), correct the bound derivation or the
fixture expectation — never widen the ceiling or soften the post-final
check — and re-derive the certification artifacts if any policy constant
changes (append-only rules: that is a policy ID bump).

## Constraints

Complete-file delivery. Do not expand scope. The suite must be fully
green including `TONEPOET_REQUIRE_TOOLS=1` qualification and the new
default-settings live-smoke test; zero cold warnings; the D1 resolution
must leave non-DSD conversions untouched.
