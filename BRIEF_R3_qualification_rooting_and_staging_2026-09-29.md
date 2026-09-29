# Brief R3 — Reference cannot be qualified at all right now

Date: 2026-09-29
Base: `main` @ `e8095f3`

Problems 1 and 3 are closed and verified. Problem 2's code is in and the gate is
deterministic. But Reference qualification cannot currently be run, so the
portable work cannot be proved and two gate tests stay red.

Two outcomes are required. They are complementary, not alternatives.

---

## Outcome 1 — a Nix deployment can be qualified

Qualification now requires a staged package runtime tree. Nix does not need one
and cannot easily provide one, so the Nix path is blocked by a requirement that
does not apply to it.

A Nix binary cache hands every user the identical absolute `/nix/store` paths,
serving byte-identical contents for each. There is nothing to relocate, and the
executable hashes, closure digests, and behavior probes that already bind those
bytes are recorded independently of any manifest.

Two things in the qualification harness enforce the staged tree. It refuses to
start without an attested manifest, whose digest feeds the qualification
identity. And every bound executable must resolve inside the closure root.

A partial change is already on `main`: the closure root defaults to
`/nix/store` when no staged root is named, and the evidence records which
rooting produced it. That addresses the second of those only. Keep it, replace
it, or discard it as you see fit.

### Required

A Nix build on a qualified build host can be requalified, and the evidence it
produces is honest about what it attests. Whatever replaces the manifest digest
in the Nix rooting attests the same thing the manifest attests: the exact bytes
that will execute.

Do not make the staged-package path weaker to achieve this. A package whose
users do not share our store paths must still be qualified against its own
staged tree, and evidence produced under one rooting must not silently satisfy
the other.

---

## Outcome 2 — a relocatable package runtime tree can be built

`.deb` and `.rpm` are the distribution target. Those users have no `/nix/store`.
The staged-tree contract exists for them, but nothing can produce a tree that
satisfies it.

Attempted here, from the real closure of the six bound executables — sox,
ffmpeg, ffprobe, metaflac, wvtag, AtomicParsley — 374 store paths, 1.5 GB:

```
RuntimeError: absolute symlink target in closure:
  nix/store/...-getent-glibc-2.42-51/bin/getent -> /nix/store/...-glibc-...-getent/bin/getent
```

The manifest builder rejects absolute symlink targets, correctly — it is a
fail-closed escape check. The staged copy had 3,688 symlinks, 12 of them
absolute. Rewriting those 12 would not be enough: every ELF in the closure
carries an absolute `/nix/store` interpreter and RPATH, so the tree is not
relocatable and a manifest over it would attest bytes the loader never reads.

### Required

A repeatable, reviewable step turns the qualified Nix closure into a relocatable
package-private runtime tree that the existing manifest builder accepts and that
actually runs with nothing outside it. Every mutation it performs happens before
the manifest is built, per the existing contract.

It is tooling, not a one-off: the tree is rebuilt whenever the pinned tools
change, and the qualification that follows must match the bytes that ship.

---

## Sequencing

Outcome 1 unblocks the gate and Nix distribution now. Outcome 2 unblocks
`.deb`/`.rpm` and is required regardless. Either can land first.

Requalification itself runs on our build host, not in your environment.

## State on this machine

`main` @ `e8095f3`. Gate: 7192 passed / 2 failed across two identical
consecutive runs, zero lease-size errors. `tonepoet-true-peak`: 160/0.
`cargo check --workspace --all-targets` clean.

The two failures are the qualification refusing stale promoted evidence and are
exactly the debt this brief is about:

- `convert::pipeline::track_executor::tests::phase5_promotion_binds_core_and_optional_metadata_closure_variants`
- `convert::pipeline::track_executor::tests::production_promoted_evidence_refuses_missing_sox_before_any_tool_launch`

Promoted evidence is untouched and stays stale until a real qualification runs.

## Build capability, unchanged

`cargo test -p tonepoet-pipeline` (~9 s) and `cargo test -p tonepoet-true-peak`
(~3 s to build) need no audio tools and compile anywhere. The root `tonepoet`
crate peaks at 6.24 GB in a single `rustc` even at `-j 1` with debug info off,
so it will OOM under a 4 GB ceiling — write those changes uncompiled and say
which ones they are. R1 shipped a call to a function that does not exist in a
crate it could not build; R2, which touched the same area, compiled clean.

## Outstanding, not in scope

The R6 handoff still owes the 4,368-cell physical SSRC grid and one real WavPack
decode comparison. Issues #55 and #56 are filed and unrelated to this brief.
