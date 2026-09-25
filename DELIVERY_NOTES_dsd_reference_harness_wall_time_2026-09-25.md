# Delivery notes: DSD Reference harness wall time

Date: 2026-09-25
Base bundle head: `2eb561d`
Scope: `BRIEF_dsd_reference_harness_wall_time_2026-09-25.md`

## Result

This delivery addresses the two measured wall-time causes in the brief without changing Reference numerical policy, the true-peak crate, SSRC evidence, or installed qualification evidence.

### 1. External-tool launch overhead

`RealToolRunner` now reuses the existing long-lived execution-supervisor primitive for ordinary direct tool calls that carry no durable lifetime descriptors. Queue-item calls continue to use their item supervisor. Calls that carry command-scoped lifetime descriptors continue to use a dedicated helper, so a process-global helper can never extend an execution/path/staging lease beyond its owner.

The containment backend no longer re-execs the full Tonepoet binary as `__action-script-launcher` for every external command. The already-single-threaded containment helper/backend worker now forks a small trusted exec gate and runs the existing `run_internal_launcher` path there. Linux cgroup placement, parent-death signaling, process-group/session setup, exact retained-file execution, timeout/cancellation, descendant tracking/reaping, and the final CLOEXEC boundary remain in the existing containment paths.

Both supervised client paths also no longer pay the unconditional 100 ms post-result lifecycle sleep. Lifecycle events are synchronously acknowledged before the helper can publish the terminal result, so the fixed sleep cannot preserve an in-flight event. The independent bounded stdout/stderr tail-drain remains unchanged.

The shared helper checks its child between commands and replaces an exited helper before the next command. It deliberately does not retry a command after submission; an external command with side effects can therefore never be executed twice by recovery logic.

### 2. Qualification scheduling

The 3,540 positive package cells are unchanged. Scheduling changes from ten sample-rate work items to 300 `sample-rate x channel-count x PCM-depth` work items. Each group executes the existing target/level cell body and existing production `execute_reference_common_plan` path. The full-matrix evidence aggregation and exact acceptance counts remain unchanged.

The default Rayon pool still uses `available_parallelism()`, but is no longer capped at ten sample rates. `TONEPOET_QUAL_JOBS` remains supported. This removes the 10-of-16-core ceiling and allows work stealing across the two slow highest-rate tails without introducing per-cell architectural churn.

## Changed files relative to bundled head

Exactly three repository files differ from the supplied bundle:

- `src/convert/script_supervisor.rs`
- `src/convert/pipeline/tool.rs`
- `tests/dsd_reference_qualification.rs`

`CHANGED_FILES.txt` is authoritative. The included patch applies those exact changes against the supplied head.

## Reference evidence / requalification decision

No file in the current Reference common source-lock set changed. Recomputing `build.rs`'s source-lock algorithm before and after this delivery yields the same digest:

`f6d46af27341ae784f695008ed58e23ab5b2bf5d475c16192cb241c88d3e76c6`

The complete `crates/tonepoet-true-peak` source/manifest digest is likewise unchanged:

`3ec272e297b1b580bb5b41f1691a687d97df9e39722aa9f1a996e2c08f568049`

The SSRC Binary64 registry/evidence and `tonepoet-pipeline/qualification` evidence files are byte-for-byte untouched. Therefore this delivery does **not** require v18 report/certification/evidence regeneration solely because of a source-lock change, and it does not hand-edit or reinstall any of those artifacts.

The operator should still run the complete 3,540-cell qualification after applying this delivery. That run is the functional and performance acceptance test for the changed execution machinery; it is distinct from source-lock-triggered evidence regeneration.

## Validation performed in this environment

- Repository comparison: only the three files in `CHANGED_FILES.txt` differ from the supplied bundle.
- `git diff --no-index --check` on all three changed files: clean.
- Structural delimiter scan of all three modified Rust files: clean.
- Reference common source-lock digest: unchanged, exact value above.
- True-peak source digest: unchanged, exact value above.
- `tonepoet-pipeline/qualification`: unchanged.
- `crates/tonepoet-true-peak`: unchanged.
- Existing concurrency corrective static sentinels have identical before/after status:
  - round 1: fail before / fail after (`unlocked zero-length descriptors self-heal`), pre-existing at bundled head.
  - round 4: fail before / fail after (`quiescent v23 activation regression exists`), pre-existing at bundled head.
  - round 5: pass before / pass after.
  - round 6: fail before / fail after (`substring not found`), pre-existing at bundled head.
  - round 6-r1: pass before / pass after.
  - round 7: pass before / pass after.

This execution environment has no Rust/Cargo/rustfmt/Nix toolchain, so I did not claim a compile, workspace gate, Stage A timing, full qualification wall time, or peak-memory result here. The brief explicitly places the gate on the operator side.

## Operator acceptance

Run the repository's pinned development environment and the gate command specified by the project, using the brief's current baseline of 7,058 passed / 0 failed / 16 ignored and zero warnings rather than older counts in architecture-context documentation.

Then rerun:

1. the Stage A external-tool profile, checking issue #37's fixed-cost target and Opus/AAC Metadata timing;
2. the complete 3,540-cell Reference qualification with the production executor;
3. wall-time, CPU utilization, and peak system-memory observation on the same machine used for the brief.

The implementation removes the measured full-binary re-exec costs, removes the unconditional 100 ms lifecycle tax, and removes the ten-rate scheduler ceiling. Those are structural changes; the brief's numerical performance targets remain empirical acceptance criteria and are not asserted without an operator-side measurement.
