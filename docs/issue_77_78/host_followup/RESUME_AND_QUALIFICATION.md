# TonePoet issues #77 / #78: host-results follow-up and resumable handoff

- **Date:** 2026-10-27 (date of the reported host results).
- **State:** follow-up candidate packaged; **NOT** a fully runtime-qualified release.
- **Resume entry point:** read this file, then `HOST_RESULTS_2026-10-27.md`, then `../INPUT_BRIEF.md` and `../HANDOFF.md`.

## Source identities and provenance

- Actual source used for this follow-up: `TonePoet_SNAPSHOT_cbc976f_2026-10-27.tar.gz`, SHA-256 **`a24604be66538721f70a75881e529d468d166e290bfa018c007800f2b95e2214`**.
- Host results: `NOTE_77_78_RESULTS_2026-10-27.md`, reproduced verbatim here as `HOST_RESULTS_2026-10-27.md`.
- The original `INPUT_BRIEF.md` refers to earlier source `TonePoet_SNAPSHOT_6ad6bb1_2026-10-27.tar.gz` (SHA-256 `e8cab5cf4e5066fc82e57b8a1bee0cd679c12360e235ae68d377b189a5cb8463`). **Do not substitute this older archive for the actual `cbc976f` follow-up input.**
- The uploaded source snapshot did not contain a `.git` directory. The separately supplied Git bundle records a synthetic baseline import plus the follow-up commit; it does **not** purport to reproduce upstream Git ancestry.

## Observed host outcome, before follow-up modifications

The host tester reports that the delivered code compiled on rustc 1.93.1, the targeted tests passed, and the larger gate had **7,631 passes and one previously failing Reference qualification freshness test**. `subprocess_stdin_convention` initially failed because a spawned shell in `linux_descendant_snapshot_finds_grandchildren_without_host_scan` lacked `.stdin(Stdio::null())`; **this correction is present in the supplied `cbc976f` snapshot and retained here**.

At terminal 248x75 the measured idle TUI CPU dropped from 1.13% to 0.03% of a core in 30-second samples, or 0.075% over 120 seconds. At 400x110, longer 120-second samples were 0.37%-0.39%, with a draw every five seconds. Key-to-screen latency was 6-9 ms in both trees. Background probe visibility was 318 ms versus 316 ms in the input. The historical 2.0% idle observation is still unexplained; per-track progress rows, resize, mouse and image-preview routes remain unexercised in the reported measurements.

In the 12-track S1 comparison, wall time improved from 16.6-16.7 s to 14.5-14.6 s; S3 from 10.2 s to 8.5 s. Median per-call before-exec cost improved from ~57 to ~19 ms; post-exit remained ~38 ms. With ~6,950 unrelated host processes, the improved S1 wall time remained 14.7 s, compared to 21.3 s for input. Identical file sets, 12 FLAC audio MD5s/tags and CUE bytes were reported for S1/S3. **Those are observations about the previous delivery, not measurements of this follow-up.** The host CPU comparison overlapped an unrelated heavily loaded workload and is less controlled.

The crucial unresolved issue: on the host, `clone3(CLONE_INTO_CGROUP)` worked but `unshare(CLONE_NEWCGROUP)` returned `EPERM`; the cgroup branch never armed, and every relevant test ran under subreaper fallback. The cgroup monitoring code is **unqualified**.

## Follow-up implementation

1. **Avoid a known-doomed cgroup setup attempt.** On Linux, before creating the per-tool cgroup leaf, inspect `CapEff` in `/proc/self/status`. `CLONE_NEWCGROUP` needs effective `CAP_SYS_ADMIN`; the hardened branch additionally needs `CAP_SETPCAP`. Missing or unparseable requirements cause `Auto` to select the existing explicit subreaper fallback, preserving its weaker advertised confidence. `RequireLinuxCgroupV2` fails closed **before** user-script release. This is a latency optimization and a truthful capability diagnostic, **not** a mechanism to make unprivileged cgroup namespace creation work.
2. **Require delegation enforcement for kernel-backed containment.** Before accepting the cgroup path, verify `nsdelegate` on the *first* cgroup2 mount selected by the existing `current_cgroup_directory` logic. A private cgroup namespace without delegation constraints is not, on its own, a proved confinement boundary. Reject missing or indeterminate enforcement; never silently report `KernelEnforced` for it.
3. **Revoke inherited cgroup-namespace escape authority before release.** A launcher with effective `CAP_SYS_ADMIN` could otherwise `setns` into another cgroup namespace. Once the private namespace is created, drop that capability from the bounding, effective, permitted and inheritable sets; set `no_new_privs`; then read back the state. Any failure aborts before the ready/release handshake. This is intentionally restrictive for scripts running under a privileged launcher: setuid/file-capability elevation may no longer work. **The privileged cgroup branch requires dedicated host validation.**
4. **Stop unconditional parent lifecycle ACK delay.** Replace the unconditional 10 ms sleep in both `run_supervised` and persistent-item `run_supervised_via_item_supervisor` parent loops with a bounded `poll(POLLIN)`. The wait wakes promptly for events but still checks cancellation and deadlines at least once per 10 ms. After HUP/ERR, it no longer polls a permanently ready descriptor (avoids a busy loop). EINTR and invalid-descriptor handling are explicit.
5. **Qualification tests.** Add pure capability/mount parser tests, event readiness/HUP regression, unprivileged `Auto` fallback integration, and capability readback assertions for successful required cgroup runs. The required-cgroup integration test can be converted into a **strict positive gate** with `TONEPOET_TEST_REQUIRE_CGROUP=1`. Extend `../verify_host.sh` to run the containment integration suite. Include a Linux C smoke fixture to exercise analogous `poll`, `capget/capset`, capability bounding and `no_new_privs` kernel ABI independently.

Only Rust source changes are `src/convert/script_supervisor.rs` and `tests/conversion_action_runscript_containment.rs`, plus native test harness `../verify_host.sh`; no TUI, DSP, planner, output, or source-locked files were modified. This deliberately retains the already measured #77 optimization instead of introducing another unqualified UI change.

## Validation actually performed in the follow-up environment

- SHA-256 verified for the uploaded source snapshot.
- Unchanged files in all 24 `REFERENCE_COMMON_SOURCE_PATHS` individually checked byte-for-byte against the imported baseline.
- `git diff --check` and shell syntax checks (including host verification entry points).
- Native C ABI smoke test: **PASS** for Unix socket readiness/HUP and capability drop/bounding/no-new-privileges persisting across exec. This does not compile the Rust implementation, exercise the cgroup path, or prove container escape resistance.
- Packaged SHA-256 manifest and tar/git artifacts verified in the packaging session. Re-run `../verify_static.sh` after unpacking.
- **Not performed:** Rust build, Rust formatting, Rust unit/integration tests, TUI terminal measurements, conversion comparison, Linux cgroup-v2 live containment, or high-process-count A/B performance. This environment has no `rustc`/`cargo` and cannot download a Rust toolchain.

## Next host qualification steps (strict order)

### 1. Static evidence and compilation

From the repository root:

```sh
bash docs/issue_77_78/verify_static.sh
bash docs/issue_77_78/host_followup/verify_kernel_primitives.sh
bash docs/issue_77_78/verify_host.sh
```

Run with `rustc 1.93.1` or explicitly record the tested compiler; retain `.cargo/config.toml`'s `-C llvm-args=--vectorize-loops=false` workaround. Do **not** add `-C force-frame-pointers=yes` for the affected toolchain; the brief reports an LLVM vectorizer crash. Rust tests require the project's ordinary native build prerequisites. `verify_host.sh` runs the new integration suite and must not be mistaken for the whole gate.

### 2. Explicitly qualify both containment backends

On the ordinary unprivileged host, `Auto` should now bypass futile cgroup launcher construction, warn about missing `CAP_SYS_ADMIN` and use `LinuxSubreaper`. `RequireLinuxCgroupV2` should refuse to release code; check that the `cgroup-ran` marker does **not** exist and that no cgroup leaf is leaked. Re-run all four submitted host fixtures (TERM-ignoring setsid descendant, self-finishing child, double-fork timeout, double-fork cancellation) and inspect lifecycle-event ordering and survivors.

For **positive cgroup branch qualification**, use an isolated test VM or controlled host providing a writable delegated cgroup-v2 subtree, `nsdelegate`, effective `CAP_SYS_ADMIN` and `CAP_SETPCAP`, and policy permitting `unshare(CLONE_NEWCGROUP)`. Do not run privileged cgroup tests against a production worker or grant these caps to the ordinary desktop TUI. In that controlled environment:

```sh
TONEPOET_TEST_REQUIRE_CGROUP=1 cargo test --locked -p tonepoet \
  --test conversion_action_runscript_containment \
  required_cgroup_either_arms_without_control_fd_leak_or_fails_before_user_code -- --nocapture
```

The environment flag turns a denied/unsupported cgroup branch into a test failure, instead of the normal cross-host allowed denial. Successful path must report `LinuxCgroupV2`, `KernelEnforced`, post-exec loss of `CAP_SYS_ADMIN` from `CapEff`, `CapPrm`, `CapBnd`, `NoNewPrivs=1`, no inherited control FD, and confirmed containment empty. Extend the controlled host matrix with malicious cgroup migration attempts, descendants in other sessions/groups, timeout/cancellation, parent death, restart recovery and many unrelated host processes. Privileged test setup and intentional cgroup escape probes must be reviewed separately from the app's ordinary test gate.

### 3. Re-run conversion-output and performance gates

Compare previous-delivery versus follow-up from isolated XDG config/data/cache paths, with identical input CUE and 12-track audio, on the same host. Run S1 and S3 multiple times, with and without ~6,950 unrelated processes, and record medians, distributions, tool runtime, pre-exec and post-exit supervised overhead, whole-tree user/system CPU, and process survivors. Verify file inventory, FLAC audio MD5, all tags, and CUE bytes. Do not call equivalent output proved merely because no DSP sources changed.

The previous observations are a **regression baseline**, not new performance claims: S1 ~14.5-14.6 s, S3 8.5 s, 19 ms pre-exec and 38 ms post-exit (medians), 14.7 s S1 under process pressure. There is no valid new follow-up improvement percentage until these reruns happen.

For #77, idle Browse on clean isolated XDG dirs at 100x30, 200x50, 248x75 and 400x110; test 30s and 120s windows. Measure key/input, resize, mouse, image preview and per-track progress. Investigate the historical ~2.0% session only with a *safe database copy* and without implicitly adopting interrupted jobs. At 400x110, confirm five-second heartbeat redraws and assess whether the sustained 0.37%-0.39% reflects the expected full-buffer rendering cost or a new source of CPU.

### 4. Full project gate and Reference qualification

Run the full repository gate on rustc 1.93.1 as defined by the project. The host report had exactly one failure, `installed_reference_qualification_binds_the_current_locked_sources`, already failing on the input. The follow-up does **not** change any of the 24 locked paths and does **not** assert that this failure is resolved. In the provided source baseline, at least the installed sidecar entries for `src/convert/pipeline/stages.rs`, `src/convert/pipeline/track_executor.rs` and `src/convert/replaygain.rs` have byte-hash divergence; investigate the project's canonicalization/qualification workflow before updating the installed qualification. **Do not quietly regenerate or bless a new DSP Reference qualification in this task.**

## Conservative release decision

**Do not claim a world-class production signoff yet.** #77 is strongly improved on the reported host but the 2.0% historical observation and untested UI routes remain open. #78 host performance improved and fallback containment fixtures passed, but cgroup-backed supervision has never been exercised on that host; the new hardening branch is additionally uncompiled and unqualified here. The deliverable is a complete, byte-verifiable follow-up **candidate** with a reproducible path to the necessary host gate, not a validated release.

## Recovery after a model/session interruption

Read this file and verify the full-source SHA-256 manifest first. Use the **same archived working tree**, not a fresh original brief. If the external Git bundle is available, `git clone <bundle-file> tonepoet-followup` reconstructs the synthetic snapshot baseline and follow-up history. Continue from the follow-up commit and preserve any newly verified evidence under `docs/issue_77_78/host_followup/`. When you perform a host gate, record compiler, kernel, privilege/cgroup-mount conditions, exact commands, outcomes, A/B output checksums, benchmark data, and any failed fixtures. Do not convert skipped kernel qualification into a pass. Package every correction with a new content hash and an updated resume document.
