# CUE architecture decision ledger

| Decision | Date | Status | Reason | Gate for revision |
|---|---|---|---|---|
| ADR-CUE-PAR-001: S4 minimal S1/S3 hybrid | 2026-10-22 | **Proposed; conditional** | Existing S1 already decodes lossless exact CUE once but encodes per-track serially. S3 can overlap tracks **if** exact independent FLAC seek and global process budget are proved. S2 requires buffering to overlap future-track encoders; avoid that premature complication. | IP-02 exact seek truth; IP-03 budget; IP-05 RG/publication; IP-06 net-wall speed |
| Keep S1 and staged-carrier routes as reference | 2026-10-22 | **Preserve** | They own current boundary semantics, fallback, special input types and failed plan routes. | No removal without a separately accepted parity/rollback gate |
| First format/source: exact PCM FLAC image | 2026-10-22 | **Pilot only** | FLAC sample count via STREAMINFO and existing descriptive stream; narrow enough to test. | IP-02 evidence by codec variant/tool version/host |
| First destination: unchanged rate/depth FLAC | 2026-10-22 | **Pilot only** | Reuse existing single-command planner, no DSP reinterpretation. | IP-04/05 tool/metadata/validation parity |
| ReplayGain Both is the main benchmark case | 2026-10-22 | **Required** | UI pill begins at Both; S1 Unix source-pass mirror differs in execution strategy from post-output native scan. | Qualify signal/RG parity + total wall time with default Both |
| Additional codecs/formats | 2026-10-22 | **Deferred** | No established independent exact seek/decoder priming contract or demonstrated need. | See `provisional/INDEX.md` |

**Change protocol:** append dated ADR entries with status `proposed/accepted/superseded/rejected`, source hash, evidence references and alternatives; never silently rewrite prior test facts. A failed FLAC seek gate explicitly returns the S3 architecture decision to `rejected/pending redesign` rather than weakening the accuracy contract. See [02_ARCHITECTURE_DECISION.md](02_ARCHITECTURE_DECISION.md) for comparative reasoning.

## R2 independent-review amendment (2026-10-22; proposed, not built)

The S4 FLAC-only pilot remains selected, but the S3 runner is explicitly Unix-only at baseline and **must be gated before spawn** on unsupported platforms. IP-03 is reduced from a global weighted resource manager to a narrow scoped adapter over the processor's existing shared tool-family semaphores, counting two simultaneously live FFmpeg children as two permits for S3. Other production routes are unchanged absent a demonstrated conflict. Performance auto-selection requires same-settings hashes and a representative musical/compressible workload in addition to synthetic boundary tests. See `06_INDEPENDENT_ADVERSARIAL_AUDIT.md`. No source code has changed.

## Host gate amendment (2026-10-22; build host, proposed, not built)

The architecture is unchanged. IP-06's **decision procedure** is corrected for a
false-negative identified on the build host and confirmed in planning review.

S1's direct CUE route can observe ReplayGain inside the decode it already performs when
the source-pass predicate holds; an initial S3 worker cannot reuse that shared sequential
mirror and therefore pays an extra post-output scan under the Convert UI default `Both`.
Without an attribution step, IP-06 could record that scan's cost as evidence that ranged
decoding and parallel encoding are inherently slower, and reject a design that would
qualify once the observer exists.

IP-06 now requires: observing whether S1 actually took the source-pass path in each run;
matched RG `Off`/`Both` cells with equal configuration hashes; S3's post-output scan
reported as its own interval; and separate RG-Off and RG-Both speedups. It gains a third
verdict, **deferred**, which preserves the opt-in pilot, promotes the `PX-C` source-pass
ReplayGain observer into one focused packet built on the existing native measurement and
album-reduction machinery, and repeats qualification before any final verdict. The
observer is not to be implemented speculatively, and S3 is not promoted without
demonstrated end-to-end improvement with ReplayGain `Both`.

See `08_HOST_GATE_AMENDMENT.md`. No source code has changed and no packet IDs, scopes or
dependencies were altered.
