# TonePoet issues 77 and 78: corrective implementation and handoff

**Date:** 2026-10-27  
**Input:** `TonePoet_SNAPSHOT_6ad6bb1_2026-10-27.tar.gz`  
**Verified input SHA-256:** `e8cab5cf4e5066fc82e57b8a1bee0cd679c12360e235ae68d377b189a5cb8463`  
**Status:** Implemented candidate. Static checks passed. Host compilation, runtime safety qualification, conversion equivalence, and performance acceptance remain open.

## Intent and changes

The source archive is the sole implementation base. Only these Rust production files differ:

- `src/tui/event_loop.rs`: Skip a terminal draw when no user event, async reducer result, visible state change, or forced redraw requires one. Keep the existing 100 ms input/message poll, and use a 5 s low-frequency heartbeat for state without a signal. A forced redraw still clears the terminal; normal progress never does.
- `src/tui/app.rs`: Report changed queue snapshots, including status and individual track progress, and whether image-preview protocol preparation modified visible state. The event loop also observes changes in status text, screen, and overlay type, and it marks post-draw image protocol changes for a new non-clearing frame.
- `src/convert/script_supervisor.rs`: On Linux, attempt `clone3(CLONE_INTO_CGROUP)` to create the gated launcher in its delegated leaf without a separate `cgroup.procs` migration. If clone3 is unsupported or blocked, retain the existing fork-and-migrate path. In both cases, the launcher must root a private cgroup namespace and acknowledge readiness before the script can execute. Cgroup-backed normal monitoring uses kernel `populated` evidence instead of scanning `/proc`. Fallback subreaper monitoring skips procfs discovery while the leader runs normally, then observes descendants when the leader exits or termination starts. Descendant enumeration uses `/proc/<pid>/task/<tid>/children` when present and the original host-wide, PID-start-validated scan otherwise. Exceptional cgroup termination retains descendant tracking, including the case where `cgroup.kill` is absent and recursive cleanup needs PIDFD signalling.

All source-lock paths in `REFERENCE_COMMON_SOURCE_PATHS` remain unchanged. DSP and conversion planning code remain unchanged. The source patch is in `SOURCE_PATCH.diff`, and `SOURCE_TREE_SHA256SUMS.txt` covers the packaged files other than itself.

## Correctness boundaries

Linux cgroup-v2 is still the preferred kernel-enforced containment backend. Direct placement is not a bypass: the child cannot accept an invocation before cgroup namespace isolation and the same authenticated release gate complete. A clone3 failure does not release the script. The fallback still joins its leaf before release, or refuses/falls back according to the existing containment preference.

The process-tree fallback remains explicitly weaker than kernel cgroup containment for scripts that use unrelated external service managers. The correction does not expand that advertised guarantee. Descendants still receive PID/start-time validation and PIDFD signalling. An orphaned daemon is adopted by the dedicated child subreaper when the leader exits, including after a new session/group is created. The full process-table fallback remains available when the optional procfs children interface is missing.

No streaming, DSP, filename, tagging, publication, or output-format logic was modified. That is a structural compatibility argument, not a substitute for output-byte comparisons.

## Verified in this execution environment

- Input archive SHA-256 matched the stated source identity. Extraction succeeded.
- Only the three Rust production files listed above differ from the input. A unified patch replays on the original files. Patch idempotency and the packaged-file hashes are checked during packaging.
- Native kernel fixture: a forked process detached with `setsid`, its parent exited, and the orphan was demonstrably reparented to the test subreaper. PASS, with the orphan killed and reaped after the observation.
- The execution environment uses Linux 6.18.55 and does **not** expose `/proc/<pid>/task/<tid>/children`. A native probe confirmed this. The implementation treats that as an unavailable interface and uses the full procfs fallback at the few points where descendant discovery remains necessary.
- The raw clone3 syscall ABI was probed with an invalid cgroup FD: it failed with `EINVAL` and did not create a child. This does **not** qualify successful CLONE_INTO_CGROUP placement or its performance.
- This environment has no `cargo`, `rustc`, or `rustfmt`, and cannot resolve the public Rust distribution host. Rust compilation, Rust unit tests, formatting checks, native TonePoet conversions, TUI-in-terminal measurements, and real delegated-cgroup launch tests have **not** run.

## Remaining acceptance gates, in order

1. On the user's Rust 1.93.1 host, run `docs/issue_77_78/verify_host.sh`. Keep the repository's existing loop-vectorizer workaround. Do not add `-C force-frame-pointers=yes` on the affected toolchain.
2. Exercise the Linux supervisor with both forced subreaper fallback and real delegated cgroup containment. Test ordinary completion, a nonzero exit, timeout, cancellation, parent disappearance, a forked descendant that calls `setsid`, and a background child left after leader exit. Repeat with many unrelated idle host processes. Confirm cleanup and complete lifecycle records.
3. Confirm whether that host permits clone3 direct placement (a trace of `clone3` and `cgroup.procs` is useful for syscall selection, **not** elapsed-time measurement). If clone3 fails, the old migration still occurs; do not claim that part of #78 has been resolved on the host.
4. Repeat the original 12-track S1 and S3 CUE runs, using the same tools and settings. For the 38 ffprobe/metaflac calls, compare runner time, shim-observed tool time, supervision time before exec and after exit, helper CPU, and host-process sensitivity. Run timings without strace. Compare output content and metadata against the unmodified input tree.
5. Run an idle TUI against isolated XDG directories and a disposable, non-executable database at 100x30, 200x50, 248x75 and 400x110. Measure `/proc` CPU time with `measure_idle_cpu.sh`. Check keyboard repeat, mouse, resize, status expiration, async Browse results, overlay opening, queue progress and per-track progress. **Do not** open the user's live queue database for this test, because recovery could make interrupted work eligible for user action or a running transfer.

The original live session's 2.0% CPU observation remains unexplained above the 1.1% reproduced at 248x75 in the supplied brief. The missing evidence is a controlled same-size comparison against an isolated copy of the 41-row queue database with automatic work acquisition disabled, including process/thread profiling. The lifetime CPU total of the older session does not measure its idle rate.

## Resume protocol

This complete source archive plus `INPUT_BRIEF.md`, `SOURCE_PATCH.diff`, `SOURCE_TREE_SHA256SUMS.txt`, this handoff, and the host verification scripts are the handoff artifacts. A later session can verify the archive and source tree, run the remaining acceptance gates, and make the smallest corrections justified by the actual results. Do not return to an older TonePoet snapshot. No post-implementation runtime performance figures are asserted in this packet.
