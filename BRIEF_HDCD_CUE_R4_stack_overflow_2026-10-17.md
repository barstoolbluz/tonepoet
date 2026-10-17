# BRIEF — two CUE test binaries abort with stack overflow

R3 applied. The plan-terminal adjudication is right and the five warnings are
gone. Reference was requalified and the freshness gate passes.

Two test binaries abort rather than fail:

```
depth_format_matrix               source_target_preserves_float32_cue_sample_class
unified_synthetic_cue_output_boundary   explicit_single_cue_bypass_keeps_side_album_identity

thread '...' has overflowed its stack
fatal runtime error: stack overflow, aborting   (SIGABRT)
```

Both reproduce every time. Both are CUE paths.

Counting only the binaries that completed, the gate is 7517 passed / 1 failed,
and that one failure passes alone and is the documented contention flake.

I believe these arrived with HDCD/CUE R2: the abort appears in the raw logs of
the R2 and R3 gates and in no earlier gate, including a clean run on the tree
immediately before the HDCD work. I have not bisected, so that is inference
from the logs rather than something I proved.

**What we want:** both binaries run to completion. A stack overflow in a CUE
path is a crash, not a failed assertion, so I would treat it as reachable from
a real conversion until shown otherwise.

## I did not report this for two rounds

My gate script summed the `passed` and `failed` counts and read the `failures:`
block. An aborted process emits neither, so the aborts were invisible and I
reported those two gates as 7516 / 2 and 7517 / 1 with no mention of them. The
evidence was in the logs both times. The script now counts aborts, and the
repository instructions have been corrected.

This also means my gate numbers before those two rounds carry the same caveat.
I checked the logs I still have and none contain an abort, but older runs whose
logs are gone cannot be rechecked.

## Still needing the user

The five R5 UI changes have never been looked at. The SACD tag-transfer round
trip has never run against a real disc. The RF64 album has not been opened
since that read path landed.
