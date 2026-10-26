# IP-06 decision record — 2026-10-25

**Decision: REVERT proposed default-ON S3 promotion. Preserve opt-in S3 and the supplied host measurements.**

**Disposition:** Promotion **NOT ACCEPTED**; S3 itself **NOT REJECTED**. The only production correction is the exact reverse of `S3_PROMOTION.patch`. Unset or non-`1` `TONEPOET_CUE_S3_PILOT` again selects S1; `TONEPOET_CUE_S3_PILOT=1` requests S3 subject to the existing complete-album preflight. No planner, ReplayGain, media encoding, publisher or child-supervisor implementation is changed. This is a reviewed rollback candidate, pending native host confirmation.

## Inputs and provenance

- Supplied source: `TonePoet_SNAPSHOT_4d6b04c_s3promote_2026-10-25.tar.gz`; its SHA-256 was independently verified as `1f1fcf261ac35aa585580489a64989541a71916af58f88bc592f0a91aa196d2e`.
- Supplied change: `S3_PROMOTION.patch`; reverse application to that source is clean. The source tar has no `.git`: reported branch `s3-promote`, commit `4d6b04c`, base `379bdcc` and two-commit divergence are **NOT independently verified**.
- Governing plan: `planning_authority/packets/IP-06_qualify_and_promote.md` with the overriding `ip07_verification/IP-06_SEQUENCING_AMENDMENT.md`. Their historical contents and checksums are unchanged.
- The provided narrative reports build-host results (`7627` passed, one unrelated Reference failure) and IP-07 source-meter selection. This environment has neither the build-host logs nor the original music fixtures or a usable Rust toolchain; those assertions are **reported, not independently reproduced**.

## Positively established

1. The candidate selector returns true when the environment variable is absent or any value except trimmed `0`. It broadens eligibility to every platform, tool version, CPU and filesystem where the *functional* preflight succeeds. The preflight establishes exact FLAC+CUE/identity conditions and process family capacity; it does **not** establish tested CPU/storage/tool-version performance cohorts or a minimum effective track duration.
2. The uploaded 140-row CSV has exactly seven workloads × two ReplayGain modes × five complete matched repetitions × two nominal routes. All 140 **recorded** exit codes are zero. Independent recomputation reproduces the reported timing ratios to rounding. For twelve-track 16/44.1, reported S1/S3 median ratios are 1.44x (Off) and 1.78x (Both); the single real-album row is 1.39x/1.70x. The one-track case is an S1/S1 refusal control, not an S3 win/loss. These are useful host measurements, **not independent conversion reproductions**.
3. The cold `sshfs` spinning-disk table has four albums, three observations each, with alternate initial routes. It describes that host's SSHFS/remote-cache setup, not a controlled directly attached HDD with representative memory pressure. It does not justify auto-admission on every storage class.
4. The source change can be reversed without modifying S3's implementation. The rollback restores the exact opt-in expression from before the proposed patch.

## Why default-ON fails IP-06

- **Policy not bounded to proven cohorts (decisive):** preflight admits functionally eligible albums on unmeasured hardware and storage. IP-06 expressly requires unobserved classes to retain S1 and forbids a global promotion based on a single measured host.
- **Inadequate machine-auditable performance attribution:** the submitted `RESULTS.csv` holds only `cell,rep,route,rg,wall_s,rc,tracks`. It has no source/fixture/CUE hashes, route-excluded canonical configuration digest, build/tool identities per run, selected runtime route, peak RSS/FD/temp/children, audio/tag/file manifests, source-pass/post-output timings or source-meter fallbacks. Its wall value is documented as process-start-to-exit rather than admission-to-visible-publication. The official evaluator cannot accept that input. The single traced RG Both pilot route in `SUMMARY.md` does not provide per-cell attribution.
- **Fault and equivalence evidence not complete for promotion:** PCM/hash/tag/file-set identity and two adverse cases are reported in prose, without source-bound run artifacts in the snapshot. IP-06 additionally calls for artwork on/off, disk-full, early encoder exit, blocked pipe, concurrently contended albums, scan/rename/metadata failures, resource high-water and cancellation across required states. These are not independently established by the supplied evidence.
- **Unknown Git and native qualification identity:** no Git history or signed, source-bound acceptance packet is attached. The brief reports Rust gates on another machine; this environment cannot independently confirm them for this source snapshot.

No evidence shows that S3 produces incorrect output or that a measured eligible album is slower. The missing qualifications are grounds to **withhold a default promotion**, not to call the optimization defective.

## Corrective scope and future decision

1. Reverse the 2-function/default-selector patch and its test assertions. Preserve the explicit opt-in pilot, old S1 path and IP-07 concurrent source-range ReplayGain machinery exactly as supplied.
2. Supersede the historical trial evaluator with `ip06_verification/evaluate_trials.py`: H-01 now fails hard when a digest is reused for different ReplayGain modes or when the two halves of a pair have different configuration digests. Do not modify the immutable planning bundle. The evaluator still provides **numeric candidate screening only**; it cannot authenticate digest preimages.
3. Future IP-06 promotion requires an independently source-bound, route-paired benchmark and error/PCM/tag/publication suite on qualified host(s), with RG Both and Off compared separately, the ≥1.25x whole-run median objective and variation, actual ReplayGain paths and source-meter fallbacks, and high-water limits. Apply an **explicit, narrow, reversible admission predicate** covering only measured workload and platform classes; all other systems continue with S1. No ad hoc CPU/storage cutoffs can be justified from the present data.
4. Retain the reported one-host measurements as evidence, not as a global release approval. A product-level user-facing override can be considered separately; absence of such a control is not a correctness defect in the opt-in pilot.

**Handoff state: rollback implemented, IP-06 default admission BLOCKED / NOT ACCEPTED. Rust/native qualification remains NOT RUN here.** The exact next-source artifact name and external archive digest are in the accompanying delivery receipt. The archive's own hash cannot appear inside itself.
