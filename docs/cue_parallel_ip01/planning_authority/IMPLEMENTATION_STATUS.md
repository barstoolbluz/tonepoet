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
