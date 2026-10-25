# CUE parallel implementation status — authoritative packet states

**Recorded:** 2026-10-22 planning R2 corrective. **Baseline source:** git archive labelled `98aae2b`, verified SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. This is the **initial** packet-status ledger; no packet implemented in this planning session.

Allowed states: `planned` → `in progress` → `implemented-unqualified` → `accepted`; alternatives `blocked` / `deferred`. `accepted` requires actual applicable build, native end-to-end conformance, and the packet's safety/performance criteria on the **exact delivered full-source archive**, not simply static code reading. On rejection, revert to prior accepted source or issue a narrow corrective packet. A test not run is NOT RUN, never PASS.

| Packet | State | Consumed source | Produced full-source archive | Gate/next action |
|---|---|---|---|---|
| IP-01 | planned | Original verified snapshot | — | Opt-in source-grounded baseline trace, invariants and build-host tests |
| IP-02 | planned | **accepted** IP-01 archive | — | FLAC independent sample-exact seek + real range tests; go/no-go |
| IP-03 | planned | **accepted** IP-02 archive | — | Reuse shared per-family permits for S3 two-child pipeline; no new global reservation framework |
| IP-04 | planned | **accepted** IP-03 archive | — | Private parallel worker only, valid staged artifacts |
| IP-05 | planned | **accepted** IP-04 archive | — | Default-OFF end-to-end pilot + RG/log/pub semantics |
| IP-06 | planned | **accepted** IP-05 archive | — | Host paired full wall-time decision with ReplayGain attribution; qualify / reject / **deferred-pending-PX-C-observer**; conditional narrow default |
| PX-A/B/C/D | deferred | See provisional/INDEX | — | No new prompt until measured need and format proof |

**Authority rule:** Keep this ledger, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, and `NEXT_SESSION_HANDOFF.md` together in each cumulative implementation bundle, preserving history rather than resetting statuses. When upstream/source changes make a packet's text obsolete, update an explicit decision and next handoff, not its immutable historical ID. Do not change status to accepted until the receiving build host verifies its exact artifact hash and signed gate evidence.

**Host gate amendment, 2026-10-22:** IP-06's decision procedure was corrected on the build host to attribute S3's additional post-output ReplayGain scan before any rejection, and gained a `deferred` verdict that promotes the PX-C observer. Packet IDs, scopes and dependencies are unchanged. See `08_HOST_GATE_AMENDMENT.md`.


## Cumulative IP-02 update (2026-10-24; supersedes the historical planning-only states above)

**Source authority:** supplied `TonePoet_SOURCE_IDENTITY_IP02_2026-10-24.md` identifies input commit `847fc5e`, complete snapshot SHA-256 `880c5e2693ee09ef56723c0ac7b72cbd6080340c08671f0f36e9288b9a994112`, and reports IP-01 accepted on the build host. The input snapshot was independently authenticated; it has no `.git` and therefore the host's acceptance/clean-branch claims are **reported, not reproduced here**. The source also includes the accepted ReplayGain FIFO polling correction.

**Current production-tree digest:** SHA-256 `f97081a14dece5c1d1553cfbaf3bd60ecc02876b85fb98ba7346f7e8b0419308` over sorted `path\0SHA256(file)\n` records for 1,124 source/repository files outside `docs/cue_parallel_ip01`. This is a deterministic code-tree identity, **not the gzip archive checksum**. The complete delivery archive is `TonePoet_CUE_IP02_FULL_SOURCE_f97081a14dec.tar.gz`; verify its actual SHA-256 in the associated delivery sidecar. The archive checksum cannot self-reference inside the archive without changing itself.

| Packet | Current state | Input / output | Remaining gate |
|---|---|---|---|
| IP-01 | accepted **per supplied host identity letter** | Integrated into 847fc5e input; prior separate acceptance hash not supplied | No IP-01 modifications here; retain any host acceptance evidence separately |
| IP-02 | **implemented-unqualified** | `TonePoet_SNAPSHOT_847fc5e_2026-10-24.tar.gz` → `TonePoet_CUE_IP02_FULL_SOURCE_f97081a14dec.tar.gz` | Rust toolchain absent. Build-host `cargo fmt`, named Rust tests, `cue_stream`, `cue`, full workspace, native matched tool/runtime fixtures bound to final archive SHA, then owner acceptance |
| IP-03 | planned; **not started** | Must consume *accepted* IP-02 full source | Do not begin until IP-02 is accepted, including Unix-only process supervision and shared-budget gate |
| IP-04–06 | planned; **not started** | See immutable packet series | Do not activate S3 by interpreting these native seeks as worker qualification |

Native FFmpeg/FLAC standalone tests and checksums are in `docs/cue_parallel_ip01/ip02_verification/`. **These are not TonePoet Rust/test-run acceptance and do not unlock IP-03 automatically.** The exact source archive filename and delivery archive SHA are in the external delivery sidecar accompanying the complete archive.
