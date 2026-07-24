# Multi-FILE CUE v5 static-review corrective report

Date: 2026-07-24
Input archive: `multifile_cue_v5_robust_corrected_2026-07-24.tar.gz`

## Approval posture

This delivery is prepared for rigorous static review only. The environment contains no Rust toolchain and no Nix executable. Accordingly, this report does **not** claim that formatting, compilation, Clippy, or tests passed.

The strongest supported conclusion is stated at the end of this report.

## Contract correction: A

The previous A fixture was replaced because it conflated directory co-location with album membership. That contract was destructively overbroad and materially underspecified.

The implementation now has two explicit contracts:

- **A1 — proven merged group fails closed.** A grouping decision captures complete provenance while all CUE members are readable. If a member later becomes rejected but its file-object identity and the rest of the group still revalidate, only that proven group is suppressed. Unrelated content survives.
- **A2 — unknown relationship does not fail the folder closed.** Without proof, malformed CUEs are suppressed independently. Both matching- and nonmatching-stem audio remain ordinary queue candidates, valid CUE content remains available, and a visible no-authority warning is emitted.

The exact old and replacement bodies and the product rationale are in `docs/STATIC_REVIEW_A_CONTRACT_REPLACEMENT_2026-07-24.md`.

## Provenance model and lifecycle

`SplitCueAlbumGroupingDecision` now owns a private provenance map. Callers cannot attach arbitrary cue-to-audio maps. Provenance is captured only through the shared current-member admission path while every member is readable and admissible.

Each proven member records:

- canonical path;
- regular-file and no-symlink status;
- device/inode on Unix, or creation identity where available;
- creation time where available;
- size and modification timestamp;
- parsed album title, track numbers, normalized FILE references, and INDEX 01 positions;
- identities of every resolved direct-child audio member.

Destructive suppression is allowed only after revalidation proves:

- the decision is a merge decision;
- the group is complete and same-folder;
- every current member is either admitted or structurally rejected;
- admitted CUEs are the same file objects and have the same parsed membership fingerprint;
- admitted audio path sets and snapshots still match;
- rejected CUEs are still the same file objects;
- any audio still occupying a proven member path matches its captured snapshot;
- at least one member remains admitted and at least one member is rejected.

Missing, incomplete, stale, replaced, cross-folder, identity-less, or inconsistent provenance is ignored. The fallback is always non-destructive: suppress the rejected CUE independently and preserve ordinary content.

Provenance is operation/session scoped. It is cloned through the TUI cache and manager APIs but revalidated at use time. Repeated expansion does not mutate the provenance object and therefore remains idempotent.

## Production entry-point audit

- **CLI queue planning:** the evidence-aware planner seam accepts grouping decisions. The ordinary one-shot CLI explicitly supplies an empty map because it has no previous grouping session and must not fabricate proof.
- **`ConversionManager::add_directory`:** consumes the manager's operation/session evidence map. A public setter allows callers that already own a shared-policy decision to install it.
- **`ConversionManager::scan_directory`:** uses the same evidence-aware expansion and preserves its legacy Vec-only synthetic-artifact ownership boundary by cleaning and omitting transient artifacts.
- **TUI Browse folder worker:** snapshots cached decisions before spawning, augments evidence in the blocking worker through the existing title/TOC ladder, and calls the bounded grouping-aware expansion API.
- **TUI Browse non-folder queue path:** snapshots the same session decisions and calls `collect_selection_for_queue_with_grouping_decisions`.
- **Browse collection:** every multi-selection and directory branch accepts the evidence map; explicit files remain explicit.
- **Metadata editor to queue:** authoritative grouping completion stores the validated decision in the existing session cache; subsequent queue transitions consume that cache.
- **Direct expansion APIs:** grouping-aware unlimited, bounded, preserved-root, and cue-selection variants are available. Compatibility variants explicitly mean “no authoritative proof.”

No path manufactures provenance from filenames, stems, directory membership, or human-readable errors.

## B-F review

### B — EmbeddedOnly

Rejected non-explicit/error/artifact CUE paths are carried into direct-child sibling audio marking. Nested/subdirectory files are excluded. The raw-audio fallback remains queueable, and both CLI and TUI commit paths continue to convert membership in `cue_artifact_audio` to `Some(EmbeddedOnly)`.

### C — split source versus artifact

The tie-break removes only a metadata sidecar with exactly one track and one resolved image when an equally ranked split source covers that exact same single image. Multi-image, partial-overlap, subset, and disjoint sidecars are not swept into the rule.

### D — synthetic multi-FILE artifact

Two evidence levels are intentionally separate:

- complete identity-bearing provenance authorizes destructive A suppression;
- structured selection provenance from the shared album-title rung authorizes only representation continuity for a sole viable multi-FILE CUE.

An unrelated rejected sibling cannot set the synthetic-singleton flag. If complete A provenance proves a rejected member belongs to the group, A fail-closed policy takes precedence.

Synthetic creation, ownership transfer, cancellation cleanup, failure cleanup, and idempotent cleanup continue to use the existing artifact lifecycle rather than a parallel implementation.

### E — unresolved-only status

The earlier warning context is preserved. The terminal no-audio failure is appended and contains all required phrases: `no CUE`, `ordinary file/TOC discovery`, and `no supported audio files were found`.

### F — CATALOGNUMBER

A nonblank selected sidecar CUE `CATALOG` is upserted as `CATALOGNUMBER` after canonical duplicate collapse. Repeated editor construction cannot duplicate the row, an ALBUM edit does not remove it, blank/absent CATALOG does not create it, and the existing Foxy save-worker regression continues to exercise the selected-sidecar write path for CATALOGNUMBER edits.

## Surgical scope

Production changes are limited to eight Rust files listed in `docs/STATIC_REVIEW_CHANGED_FILES_2026-07-24.txt`. No `src/convert/pipeline/**` file, `db.rs`, recovery/journal code, sanitizer, or deferred matching implementation changed.

The previous robust corrective report was removed because its A conclusions and delivery counts were superseded. No unrelated test assertion was intentionally weakened. Exact test accounting is in `docs/STATIC_REVIEW_TEST_INVENTORY_2026-07-24.txt`.

## Static verification completed

- custom lexical delimiter checks on every changed Rust file;
- diff whitespace/patch checks;
- constructor, visibility, enum-match, and API-callsite review;
- production-path evidence-flow audit;
- staleness and identity-state review;
- added-line unsafe/panic/unwrap/expect/todo/unimplemented/unreachable review;
- test-fixture state and assertion-preservation review;
- deterministic-order and artifact-cleanup path review;
- scope-boundary review;
- archive, manifest, symlink/hard-link, embedded-brief, patch-application, and tree-equivalence checks documented below after packaging.

These are static inspections, not Rust compilation or execution.

## Final archive integrity

- Manifest data entries: `720`; every entry verified by SHA-256.
- Symlink ledger: exact match to the archive's sole symlink.
- Hard links: none.
- Traversal/absolute archive paths: none.
- Embedded `docs/gestalt_multifile_cue_v5_state.md`: byte-identical to the supplied input tree.
- Unified patch: applies cleanly to a fresh extraction of the supplied robust-corrected input archive.
- Patched baseline tree: byte-, mode-, and symlink-equivalent to the delivered output tree.
- Clean output re-extraction: byte-, mode-, and symlink-equivalent to the packaged tree.
- Deterministic archive rebuild: byte-identical.

## Unverified executable gates

The following were **not run** because no Rust or Nix toolchain is installed:

- Rust formatting;
- workspace compilation and all-target compilation;
- Clippy;
- unit, integration, real-tool, and full-workspace tests;
- runtime cancellation, cleanup, and idempotency execution.

A downstream maintainer must run, from a clean extraction or checkout:

```bash
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected A1/A2 and B-F regression names are listed in `docs/STATIC_REVIEW_TEST_INVENTORY_2026-07-24.txt`.

> Ready for final executable qualification, subject to the listed Rust formatting, compilation, Clippy, and full-workspace test gates.
