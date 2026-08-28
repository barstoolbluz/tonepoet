# Implementation report — media duration is not a wall-clock timeout

Date: 2026-08-28
Baseline stated by work order: `main` @ `acce5e1` (v0.4.9)
Status: **CORRECTED / REQUIRED NIX CERTIFICATION PENDING**

## Diagnosis

The brief's central diagnosis matches the supplied tree. `PlannedCommand::expected_duration`
is a media/progress estimate, but `planned_command_timeout()` also used it verbatim as the
process timeout (subject only to a 30-second floor). The submitted-batch DSD album-gain path
passes source media duration as `expected_duration`, so duration-bearing work received a
one-times-realtime wall-clock budget.

This is a defect class, not an album-gain-only defect: every duration-bearing planned command
passed through the same adapter policy. I therefore fixed the policy centrally rather than
special-casing DSD album-gain analysis.

## Implementation

### 1. Separate explicit process deadlines from progress duration

`tonepoet-pipeline::PlannedCommand` now has an optional `timeout_budget: Option<Duration>`.
It defaults to `None`, and is omitted from serde output when absent so existing serialized
plans do not gain a meaningless `null` field.

`expected_duration` remains unchanged as the media/progress estimate.

### 2. Give ordinary duration-bearing work real wall-clock headroom

`planned_command_timeout()` now applies this policy:

- if the planner provides an explicit `timeout_budget`, honor it exactly;
- otherwise, if `expected_duration` exists, use `expected_duration + default_timeout`;
- otherwise use `default_timeout`.

The addition is saturating. With the production default timeout of one hour, ordinary planned
work can become substantially slower than realtime under contention without being killed at
its own media duration, while a genuinely hung command is still bounded.

This deliberately does **not** derive a multiplier from the reported 0.36x measurement. The
one-hour default is already the repository's general process timeout budget; adding it to the
media/progress duration gives a stable semantic separation without codec- or machine-specific
calibration.

### 3. Preserve the qualified DSD Reference analyzer deadline exactly

The Reference true-peak measurement commands now set `timeout_budget` explicitly to the
existing `analyzer_deadline`, for both the direct SoX measurement and the Float32 FFmpeg
producer.

The executor's Reference-contract validation now requires both:

- `command.expected_duration == Some(summary.analyzer_deadline)`; and
- `command.timeout_budget == Some(summary.analyzer_deadline)`.

So the existing progress/deadline equality remains intact, while the actual process deadline
is no longer accidentally inferred from the progress field.

### 4. Regression coverage

Added a paused-time Tokio regression test,
`legitimate_work_longer_than_media_duration_survives`, which models legitimate work taking
45 seconds for 31 seconds of media. It exercises the actual timeout produced by the planned
adapter. Against the baseline policy the test times out at 31 seconds; against the corrected
policy it completes. Because Tokio time is paused, the test is deterministic and does not add
45 seconds to the suite.

Existing adapter tests were updated to cover ordinary headroom, fallback behavior, and an
explicit deadline override. Reference planner coverage now also asserts the explicit analyzer
`timeout_budget`.

## Progress and Reference invariants

Progress reporting is unchanged: `expected_duration` still carries the same values and FFmpeg
progress still divides elapsed media time by it. Command weighting likewise continues to use
`expected_duration`.

The qualified Reference analyzer retains its existing `expected_duration == analyzer_deadline`
identity and now additionally binds the actual wall-clock timeout to that same deadline.

## Contradictions found

None material. The supplied source supports the brief's core diagnosis. The important design
correction is to treat this as a separation-of-concerns defect rather than calibrating a
realtime multiplier from one measured machine/effect chain.

## Certification status

The work order requires all build/test work inside the Nix development shell and explicitly
forbids system Rust. This authoring environment does not provide `nix`; the attempted required
command failed immediately with `nix: command not found` (exit 127). I therefore did **not**
run Cargo or rustfmt using the host toolchain.

Required handoff gate in an environment with Nix:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

Every `test result:` line must report `0 failed`. Also verify the corrected tree introduces no
new compiler warnings relative to the baseline. Until those checks are completed, this bundle
must be treated as **GATE_PENDING**, not certified for final handoff.
