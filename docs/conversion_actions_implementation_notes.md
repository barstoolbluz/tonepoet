# Conversion Actions — Consolidated Implementation Notes (Passes 1 and 2)

This bundle starts from the completed pass-1 implementation and replaces its pathname-based built-in mutation/recovery layer with `src/convert/cap_fs.rs`.

Preserved from pass 1:

- ordered pre/post pipelines and tagged action model;
- persistence, preset/queue compatibility, batch election, and success/rerun gates;
- `:actions` and confirmed `:actions-run` flows;
- durable reports, cancellation, stop decisions, and script ambiguity handling;
- shared rename planning and complete built-in state machines.

Added in pass 2:

- retained acquisition and logical-root capabilities with validated relative operands;
- post-plan explicit exclusive materialization of absent external roots; ordinary child APIs cannot materialize roots implicitly;
- durable device/inode binding of each materialized root before child mutation;
- deterministic scope sharing for repeated actions targeting the same configured destination root;
- journal schema 3 scoped authority and monotonic generations;
- crash-safe final/write-temporary generation reconciliation;
- Linux `openat2` optimization with portable no-follow fallback;
- Linux/macOS exclusive rename and journal exchange backends;
- checked destructive rename against planned inode/type;
- descriptor-relative copy, move, delete, rename, create-folder, witness, staging, and cleanup;
- private recovery directories;
- bounded revalidated directory-FD cache;
- deterministic race hooks and performance counters.

The no-actions pipeline path remains unchanged except for imports/instantiation inside action-only branches.

See:

- `CAPABILITY_LAYER_DESIGN.md`;
- `CAPABILITY_PLATFORM_MATRIX.md`;
- `DEPENDENCY_MANIFEST_CHANGES.md`;
- `CAPABILITY_PERFORMANCE_NOTES.md`;
- `CAPABILITY_TEST_ACCEPTANCE_REPORT.md`;
- `LIMITATIONS.md`.

## Corrective round 3

Round 3 adds `src/convert/script_supervisor.rs`, replacing process-group-only script termination with an exec-gated dedicated supervisor. Linux prefers cgroup v2 plus a child subreaper and falls back before script release to PID-start-validated subreaper tracking. macOS combines an exec gate, process group, kqueue, recursive libproc scans, and start-time validation.

It also closes the pre-first-journal internal-root publication window with a durable descriptor-relative bootstrap authority file. Scope records bind its deterministic authority name; marker and authority cleanup use retained descriptors after the materialized identity is durable.

See `ROUND3_IMPLEMENTATION_NOTES.md`, `SCRIPT_CONTAINMENT_DESIGN.md`, `ROUND3_PLATFORM_MATRIX.md`, `ROUND3_TEST_ACCEPTANCE_REPORT.md`, and the updated `LIMITATIONS.md`.

## Strict `runscript` containment/recovery pass

The current tree now uses the dedicated exec-gated supervisor described in `RUNSCRIPT_CONTAINMENT_ARCHITECTURE.md`. Action journal schema 5 and script execution schema 2 durably bind the runtime-directory descriptor, selected backend, stable supervisor/leader identities, termination escalation, leader status, containment-empty proof, output terminal state, terminal classification, and cleanup.

Restart recovery now has a lifecycle observer. A Linux cgroup recovery request is persisted before TERM, forced escalation is persisted before `cgroup.kill`/SIGKILL, and empty proof is persisted after verification. Validated supervisor result schema 3 replays the exact live-run termination record into the action journal. Missing post-crash output capture is explicitly abandoned, so recovered scripts cannot be reported as ordinary success or replayed.

See:

* `RUNSCRIPT_CONTAINMENT_ARCHITECTURE.md`
* `RUNSCRIPT_PLATFORM_BACKEND_MATRIX.md`
* `RUNSCRIPT_JOURNAL_SCHEMA.md`
* `RUNSCRIPT_REAL_EXECUTION_TEST_REPORT.md`
* `RUNSCRIPT_LIMITATIONS.md`
* `RUNSCRIPT_DEPENDENCY_CHANGES.md`

### Final strict-containment corrections

The cgroup control descriptor is now closed on target exec; lifecycle ACKs are bounded and disconnect-safe; live post-start supervisor faults immediately enter stable-identity recovery; deterministic executed failures require durable leader, empty-domain, and output-terminal proof; validated never-released recovery is a non-replayable setup failure; and runtime cleanup is identity-checked and idempotent. Automatic cgroup restart recovery remains deliberately restricted to the current delegation parent.
