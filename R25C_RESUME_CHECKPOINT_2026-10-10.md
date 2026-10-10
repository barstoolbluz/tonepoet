# TonePoet R25C — resumable execution checkpoint (2026-10-10)

**Current checkpoint:** R25C source is corrected and static patch integrity verified. Compilation, workspace tests, CLI smokes, Reference qualification, and live SACD TUI #43 are unverified in this environment due to missing tools/fixtures.

## Read order / durable artifacts

1. `R25C_DELIVERY_NOTES_2026-10-10.md`: specific R25 audit defects, resolution, caveat, tests and build-host gate.
2. `R25C_CODE_CHANGES.patch`: exact one-file delta against the prior delivered R25 archive.
3. `src/convert/pipeline/stages.rs`: live source of truth for preflight and incremental recovery.
4. `R25C_CHANGED_SHA256SUMS.txt`: current source/patch/document hashes.
5. Historical `R25_DELIVERY_NOTES_2026-10-10.md`, `R25_CODE_CHANGES.patch`, and `R25_CHANGED_SHA256SUMS.txt`: provenance of the earlier delivered version, **not** the current R25C source hash authority.

## Conditions already resolved in R25C source

- Ordered `album_batch` + dispatcher `album_batch_track` allow one unoccupied sibling in an existing album dir even with `write_conversion_log=false`.
- A FAILED workspace from track A cannot authorize an existing file for track B. No staged-audio comparison happens before per-track installed durable evidence proves batch, sort key, actual output target and staged fragment alignment.
- The R23 incremental `RemoveCreatedFile` journal permits rollback and republish of the target specifically listed, even for a dead-owner crash. Without that marker or a successfully published same-batch target fragment, a dead-owner batch alone is insufficient evidence and fails closed.
- Multi-root preflight requires success fragments for every planned output; multi-root publisher retains its stricter all-fragments and all-payloads check.
- Original R25 SACD catalog provenance correction is preserved unchanged.

## Do next, on a properly equipped build host

Follow the full ordered commands in `R25C_DELIVERY_NOTES_2026-10-10.md`. If compilation/tests reveal actual source defects, make the smallest correction, add a focused regression, regenerate `R25C_CODE_CHANGES.patch` and the hash manifest, and requalify Reference **once** after all source changes. Do not broaden to redesigns without demonstrated defect. The crash window after rollback marker retirement but before out-of-album fragment installation is explicitly not claimed as recoverable by R25C; see delivery notes for why broad incumbent comparison is unsafe.

Persist test outcomes and source identities in an updated checkpoint. This work is not scheduled and no background job is running.
