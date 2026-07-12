# Conversion-action `runscript` containment architecture

## Scope

This pass starts from the descriptor/capability conversion-actions tree. It does not replace the action planner, election protocol, built-in filesystem state machines, or reporting model. It strengthens only the `runscript` runner seam, its durable lifecycle record, termination, output finalization, and restart recovery.

## Process topology

A conversion worker never executes the configured script directly. It starts a fresh Tonepoet helper through the hidden `__action-script-supervisor` command before Tokio is initialized. That supervisor starts a minimal hidden `__action-script-launcher` and retains all process-tree responsibility.

The launch protocol is deliberately gated:

1. The action engine creates a private, journal-owned runtime directory and durably records its device/inode identity.
2. The parent writes `spec.json` through the retained runtime-directory descriptor using exclusive `openat` creation and directory synchronization.
3. The supervisor creates and arms its platform containment backend.
4. The launcher acknowledges that it has joined the backend and established parent-death protection.
5. The supervisor emits `ContainmentPrepared` with the complete stable descriptor.
6. The action engine writes that descriptor and transitions the operation to `script_start_recorded` durably.
7. Only after the parent acknowledges that durable transition does the supervisor send the invocation to the launcher.
8. The launcher performs direct `exec` of the configured executable. No shell parses the path or arguments.

A cancellation or timeout observed between steps 5 and 7 is queued before the acknowledgement. User code therefore remains unreleased. The lifecycle acknowledgement channel has bounded send/receive deadlines; disconnect or timeout aborts release and triggers immediate stable-identity recovery rather than leaving a gated helper alive indefinitely.

## Runtime authority channel

The supervisor receives an inherited descriptor for the private runtime directory. It does not reopen `spec.json` or publish `result.json` through a pathname.

* `spec.json` is created with `openat(O_CREAT|O_EXCL|O_NOFOLLOW)`.
* The supervisor validates the directory owner, mode, device, and inode.
* `result.json` is first written and synchronized as a private temporary file, then published no-clobber with a same-directory hard link and directory synchronization.
* Recovery reopens the initial runtime pathname only to reacquire the already journaled directory identity; all record access after acquisition is descriptor-relative.
* Runtime cleanup is identity-checked and idempotent: an already-removed private runtime is accepted, while a pathname replacement with a different device/inode is preserved and reported as a contradiction.

## Runner and backend seams

`ActionScriptRunner` owns three operations:

* `run`: execute with a lifecycle observer whose acknowledgement is the durable exec gate;
* `recover`: inspect or terminate a recorded containment while persisting recovery lifecycle transitions before signalling;
* `cleanup`: remove backend artifacts only after terminal state and containment-empty proof are durable.

Platform code is isolated in `src/convert/script_supervisor.rs`. Conversion pipeline and TUI code see only runner types, lifecycle events, descriptors, and outcomes.

## Linux: delegated cgroup v2

The strong Linux backend is selected only when the running user can create and open a private leaf beneath its current delegated cgroup. The launcher writes itself into the leaf before it receives the invocation, then creates a private cgroup namespace rooted at that leaf before acknowledging readiness. If either operation fails, no script bytes have been released.

The backend combines:

* a private cgroup-v2 leaf;
* a private cgroup namespace that prevents the script from naming ancestor/sibling cgroups through the inherited cgroup mount;
* explicit close-on-exec on the retained cgroup control descriptor before target `exec`, so user code cannot inherit containment-management authority;
* a dedicated child subreaper;
* PID start-time validation;
* pidfd signalling where available;
* `cgroup.kill` for forced termination where supported;
* cgroup population checks plus descendant reaping before empty proof.

The cgroup path is not trusted by name alone. The descriptor records the token, host, boot, device, inode, supervisor identity, leader identity, and runtime-directory identity. Recovery reopens and validates the exact leaf before signalling. Automatic recovery also requires the recorded leaf to remain beneath the process's current delegated cgroup parent; a restart under a different delegation fails closed rather than signalling through a newly interpreted path.

## Linux: child-subreaper fallback

If automatic cgroup setup is unavailable, `Auto` falls back before user code is released. The fallback uses:

* a dedicated supervisor process;
* `PR_SET_CHILD_SUBREAPER`;
* an exec-gated launcher with `PR_SET_PDEATHSIG` and an immediate parent-identity check;
* a private session/process group;
* repeated `/proc` descendant, reparenting, and original-process-group scans;
* PID start-time validation and pidfd signalling where available;
* bounded TERM then KILL escalation;
* direct child and adopted-descendant reaping.

This is explicitly recorded as `linux_subreaper` with `process_tree_observed` confidence and a warning. It is not presented as equivalent to the cgroup backend. Full `/proc` visibility is required for its ownership scans. A script that asks an unrelated long-lived service manager or external broker to start work can escape ancestry-based ownership.

## macOS backend

macOS uses an exec-gated trusted supervisor with:

* a private session/process group;
* kqueue `EVFILT_PROC` observation for fork/exec/exit activity;
* recursive libproc child, process-group, and process-table scans;
* PID plus process-start-time identity checks;
* repeated TERM then KILL of the observed domain;
* bounded waits and explicit empty/uncertain outcomes.

macOS has no unprivileged cgroup-equivalent kernel execution domain. The backend therefore records `process_tree_observed` confidence and reports uncertainty instead of success if the observed domain or inherited output handles cannot be proven terminal. Work delegated through launchd or another unrelated broker cannot be absolutely contained by this backend.

## Leader and background-process policy

A zero leader exit is not success by itself. Success requires all of the following:

* user code was durably released;
* leader exit status is durably recorded and successful;
* the backend-owned domain is proven empty;
* output capture reaches `complete` or `truncated`, never `abandoned`;
* the success terminal state is durably committed.

If the leader exits while descendants remain beyond the short background grace period, Tonepoet records `leader_exited_with_descendants`, terminates the domain, and reports deterministic script failure. Deliberately daemonizing scripts are therefore rejected by default.

## Output handling

The script inherits bounded stdout/stderr pipes through the supervisor. Separate nonblocking tail readers retain only the configured tail limit. After the supervisor exits, readers receive a bounded final drain interval. If a foreign or unobservable process keeps a pipe open, capture is marked `abandoned`; Tonepoet does not hang indefinitely and does not report success.

## Restart recovery

No durably started script is automatically replayed.

Recovery first validates journal schema, token, runtime-directory identity, host/boot identity, descriptor, backend identity, and any existing supervisor result. It never signals a numeric PID or cgroup name without stable identity evidence.

* A surviving cgroup is a strong recovery handle. Recovery durably records `Recovery` termination before TERM, records forced escalation before `cgroup.kill`/SIGKILL, then records containment-empty proof.
* A live, identity-matched subreaper or macOS supervisor is allowed a bounded interval to publish its no-clobber result after detecting parent disconnect.
* A durable result replays its exact termination reason/deadline, forced-escalation flag, leader status, and empty proof into the action journal.
* A validated result that proves the exec gate never opened terminalizes as `setup_failed_before_execution`; it is a deterministic action failure, not cancellation, and is never replayed.
* A live supervisor failure after durable start immediately invokes the same stable-identity recovery path before returning to the action engine.
* Because the original parent owned stdout/stderr capture, restart recovery records missing capture as `abandoned` and reports manual recovery after released user code, even when the execution domain is empty.
* A missing or reused supervisor identity, foreign host/boot, vanished runtime directory, contradictory result, or unverifiable backend produces manual-recovery state without signalling.

Backend and runtime artifacts are removed only after the action journal has durably recorded a terminal state and sufficient empty proof.

## Retained reviewed-executable descriptor

The target executable is now opened no-follow and content/object-verified by the action capability layer before runtime setup. The resulting read-only descriptor is inherited by the trusted supervisor and then by the already-contained launcher. Neither process reopens the original script pathname.

The launcher executes the retained open file description with Linux `fexecve` or macOS `/dev/fd/<fd>` `execve`. The original pathname remains only in the authenticated specification for diagnostics and `argv[0]`. Close-on-exec handling is explicit: the descriptor is normally protected, cleared only across the trusted helper/launcher exec boundaries and for the final interpreter handoff required by shebang scripts.
