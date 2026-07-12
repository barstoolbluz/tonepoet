# Explicit Remaining Limitations

1. **Compilation and execution were unavailable.** The sandbox has no Rust toolchain and the supplied source remains a partial checkout. Syntax, patch, integrity, and static authority checks were performed; compilation, formatting, Clippy, tests, and native platform execution were not.

2. **Pass-1 schema-2 active journals are preserved, not guessed into descriptor authority.** Those journals never recorded retained-root identity. Automatically rebinding them after process death would reintroduce the pathname trust this work removes. They fail closed and require explicit administrative inspection/recovery.

3. **Capabilities cannot be resurrected from nowhere after process death.** If a recorded root is renamed outside the expected no-follow ancestor chain and the process holding the descriptor dies, portable Linux/macOS APIs cannot prove a newly supplied pathname names the original object. Recovery fails closed.

4. **There is no portable atomic compare-and-unlink primitive.** Cleanup reopens no-follow through retained capabilities, carries expected device/inode/type, and rechecks immediately before `unlinkat`. Private `0700` recovery roots and witness staging sharply narrow exposure, but a same-owner actor with write access can race the final syscall interval on platforms without a conditional unlink primitive.

5. **Unsupported atomic filesystem primitives fail closed.** If exclusive directory rename or atomic journal replacement is unavailable, the operation stops rather than reverting to an overwrite-capable or validate-then-mutate algorithm. This can reduce availability on unusual network/FUSE filesystems.

6. **Copy metadata parity remains the brief/companion-copy contract.** Content, permission/mode bits, and mtime are preserved and govern idempotency; directory copies apply that contract to every selected descendant. This corrective round does not add owner/group, ACL, xattr, flags, birth time, or atime equivalence.

7. **Browse file tasks are not migrated.** The reusable capability primitives remain suitable for later migration, but this bundle changes conversion actions only, as required by the pass-2 scope.

8. **macOS cannot provide cgroup-equivalent unprivileged descendant containment.** The exec gate, process group, kqueue notifications, recursive libproc scans, and PID/start validation are the strongest practical implementation here. A deliberately timed detach between all observation points cannot be proven impossible without a privileged system service.

9. **Abrupt supervisor death can still require system-level cleanup.** Linux cgroup membership remains visible to the kernel, but no in-process code can run after an uncatchable helper SIGKILL. On macOS there is no persistent kernel container. The durable action journal correctly records script-start ambiguity and never replays the script automatically.

## `runscript` containment limitations

The previous generic process-group limitation has been replaced by the platform-specific supervisor architecture. Remaining limits are explicit rather than hidden: Linux cgroup delegation may be unavailable; the Linux subreaper and macOS observer cannot absolutely contain work launched through an unrelated external broker; weak-backend recovery requires a live stable supervisor or its durable result; output capture lost with the application is marked abandoned; cross-host journals never signal locally; and no Windows Job Object backend is present in the supplied Unix-oriented tree. See `RUNSCRIPT_LIMITATIONS.md`.

## Exact preview/capability pass validation boundary

This overlay does not contain the complete module graph and the execution environment has no `cargo`, `rustc`, `rustfmt`, or Clippy installation. The new tests and platform-gated code therefore have not been compiled or executed here. Static parsing, source-level protocol checks, manifest parsing, and archive-integrity checks are not substitutes for the required complete-repository build and native Linux/macOS/NFS/sshfs/EXDEV acceptance matrix.

## Final correctness-pass limitations

10. **Retained-descriptor execution does not make a writable inode immutable.** Pathname replacement after review no longer changes which executable is launched: Linux uses `fexecve` and macOS executes the inherited `/dev/fd` object. A same-owner actor that already has write authority to the exact same inode may still modify that inode's contents after validation; eliminating that different threat requires executing a sealed immutable snapshot rather than the reviewed filesystem object.

11. **Matcher validation still enumerates the relevant album tree.** It no longer reads unrelated audio payloads, but exact detection of new or removed glob matches requires metadata enumeration. Very large or high-latency sshfs/NFS trees can still take time. Preparation is off the UI thread, checks cancellation during directory traversal, and reports granular progress.
