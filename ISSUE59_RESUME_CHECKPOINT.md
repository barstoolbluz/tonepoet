# Issue #59 reproducible continuation checkpoint

**Date:** October 17, 2026. **Input:** `TonePoet_LOGGING_59_second_instance_2026-10-17.tar.gz` and `BRIEF_59_second_instance_blocks_saves_2026-10-17.md`.

## Current status

Narrow source fix delivered: `src/concurrency.rs` adds conflict/claim-lifecycle forensic logging and focused regression tests. Claim policy is deliberately unchanged. **Rust build gate remains unrun in this environment** (no `cargo`/`rustc`/`nix`). Original incident descriptor is gone, so root-cause classification between an overly broad ancestor claim, retained execution claim, or other path-bearing live lease remains open. Do not claim the specific historical blocker was eliminated.

## Resume procedure (no need to rediscover inputs)

1. Extract the full delivery bundle; read `ISSUE59_ENGINEERING_DELIVERY.md`, then `ISSUE59_OFFLINE_QA.txt` and this checkpoint.
2. Verify `sha256sum --check ISSUE59_SOURCE_TREE_SHA256SUMS.txt`. The inherited `SHA256SUMS` was already stale, as explained in delivery notes.
3. Verify the sole preexisting code change with `ISSUE59_CODE_DELTA.patch`; inspect `src/concurrency.rs` near `claim_conflict_diagnostic`, `MutationClaimGuard::acquire_grouped_internal`, and the three tests named `issue_59_*`.
4. In the pinned Nix build environment, run `cargo test -p tonepoet --lib issue_59`, `cargo check --workspace --locked`, then the full workspace gate with `--no-fail-fast` and preserve complete results. Run Reference qualification where required by the unchanged source-lock contract.
5. Reproduce on a *live* two-TUI scenario, capturing application WARN; inspect `held_scope`, `held_path`, `owner_lifecycle`, `owner_descriptor_id`, `availability`, and `origin_pid`. Only then consider a **small**, evidence-backed claim-lifetime or action-root correction if evidence identifies one.

## Non-goals and important constraints

- Never disable conflict detection, treat active guards as merely advisory, reclaim live descriptors, or release conversion-script claims before termination/containment authority is proven.
- Do not confuse a process-wide unleased supervisor with a per-item supervisor carrying execution leases.
- Do not extend the source change into unrelated metadata, codec, DSP, or test infrastructure code without a reproduced defect.
- A pass of compile-free static checks is **not** a substitute for native tests.
