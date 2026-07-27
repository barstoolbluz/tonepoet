# Performance Restoration Engineering Report — Source-Bound Acceptance and Safe Windows Replacement

Date: 2026-07-26
Target version: 0.4.4
Delivery type: fail-closed source overlay against exact supplied preimages

## Executive outcome

This corrective overlay resolves the deterministic test contradiction, repairs the two replay tests so they exercise matching strong authority and reach the digest comparison, adds the required advanced Config-screen control, and adds end-to-end standard generic-metadata coverage with semantic read-back and explicit I/O/recovery accounting. It also fixes recovery-authority bypass on semantic no-ops, synchronizes the visual status setting with its runtime mirror, makes metadata acceptance strategy-aware across the 256-MiB guard, and removes the Windows read-only flush risk from standard single-file copies. This revision additionally keeps bounded-memory replacement temporaries restrictive for the entire copy/rewrite window, adds Windows 128-bit file-identity and hardlink-count enforcement, removes the unsafe Windows `MoveFileExW` fallback, calls `ReplaceFileW` only with supported flags, classifies its documented partial-failure states without guessing, uses verbatim long-path Win32 names, fails closed on unsupported non-Unix/non-Windows platforms, and cryptographically binds generated acceptance evidence to the exact complete tested source tree and delivered overlay diff.

The implementation retains the explicit `standard`/`strong` authority model introduced in the first delivery. Standard remains the default and removes avoidable content reads and per-file durability barriers. Strong retains the historical content-proof, post-copy verification, and journaled metadata behavior.

This environment still does not contain the complete Tonepoet workspace, a Rust toolchain, or a reduced-capability mount. Therefore this report does **not** claim that formatting, Clippy, `cargo test --workspace`, or the required release performance measurements were executed. The corrected bundle is a materially stronger handoff candidate, but final handoff certification still requires those repository- and mount-dependent gates.

## Baseline identity — resolved

The apply target is **`hardening @ 83fe80e`**, as identified by the later handoff readme that accompanied the supplied source bytes. The `839baab` identifier in the earlier brief is the feature-lineage commit that introduced the degraded-rename ladder and remains a behavioral reference; it is not this overlay's apply target.

Both governing documents are corrected accordingly:

- `docs/perf-restoration-handoff-readme.md` names `83fe80e` as the apply target and requires ancestry verification in the complete repository.
- `docs/perf-restoration-brief.md` distinguishes the `839baab` feature-lineage commit from the `83fe80e` delivery baseline.

Application remains fail-closed: every modified pre-existing file must match `PREIMAGE_SHA256SUMS` byte-for-byte. In the complete repository, the recipient must also run:

```bash
git merge-base --is-ancestor 839baab 83fe80e
```

A nonzero result is a handoff blocker.

## Corrective findings addressed

### 1. Native-move counter contradiction

The always-on test is corrected and renamed:

`same_filesystem_native_rename_has_one_stat_only_walk_and_zero_content_reads`

It now pins the intended standard-mode behavior:

- one stat-only source-manifest tree walk;
- one native rename attempt;
- zero copied bytes;
- zero source hash bytes;
- zero destination hash bytes;
- zero destination verification walks.

The report no longer describes native move as performing zero tree walks. The performance requirement is zero **content-byte reads**, not zero metadata enumeration.

### 2. Advanced/config-screen verification control

A real Config-screen visual control is now present. From the Config screen's Performance pane, Enter, `v`, or `f` opens `File Operations - Advanced`, containing:

- Verification: `standard` / `strong`;
- Status messages: `quiet` / `verbose`;
- Close progress on success: `off` / `on`.

The overlay owns a draft, commits all three values atomically on Enter, persists them through `TonepoetConfig::save`, and restores the prior configuration if persistence fails. Esc discards the draft. It is not a command-only implementation.

Pinning tests:

- `performance_config_opens_advanced_file_operation_control`
- `advanced_file_operation_control_persists_all_values_atomically`
- `advanced_file_operation_control_cancel_discards_draft`
- `advanced_file_operation_control_rolls_back_on_save_failure`

The vi command remains available:

```text
:set verification=standard
:set verification=strong
```

### 3. Performance evidence and harness hardening

No after measurements are invented. The release harness is strengthened so that, when run, it:

- refuses fixtures smaller than 16 files or 128 MiB;
- executes both standard and strong modes;
- verifies that every worker emits an authoritative completion report and completes every root;
- identifies the filesystem policy;
- emits machine-readable `PERF_RESULT` lines;
- records copy and move warning counts;
- enforces the standard strict-mount copy objective of at most 1.5x the plain-copy baseline.

The metadata release harness now:

- executes both verification modes;
- reads the resulting title back through the production editor reader;
- emits the selected Standard strategy, original and committed carrier lengths, elapsed time, I/O counters, and recovery-mutation counters;
- enforces the in-memory Standard budget at or below 256 MiB: one source pass, one replacement write, at most two syncs, zero backup bytes, and zero journal writes;
- enforces the bounded-memory Standard budget above 256 MiB: two source passes with exact parser/copy byte accounting, two replacement writes with exact copy/Lofty-write byte accounting, at most two syncs, zero backup bytes, and zero journal writes;
- proves that strong mode still performs a full-file backup and journal state transitions.

The required measurements remain a final external gate because the necessary workspace, toolchain, carrier, and mounts are unavailable here. `RUN_ACCEPTANCE.sh` now refuses the wrong mount classes and atomically generates a separately checksummed `perf-restoration-acceptance-results.md` containing the completed tables. The immutable source report is never edited after application. See **Performance measurement gate** below.

### 4. Recovery authority is checked before semantic no-op success

`write_all_tags_lofty_standard_atomic` now opens the metadata database and calls `assert_metadata_write_unarmed(path)` before parsing or deciding that the requested values are already present. An armed prepared journal or stale legacy `.tonepoet-bak` therefore refuses both mutation and semantic no-op requests.

Pinning tests:

- `standard_generic_metadata_noop_refuses_armed_journal_before_success`
- `standard_generic_metadata_noop_refuses_stale_legacy_backup_before_success`

Both tests first establish the requested tag value, seed the unresolved recovery authority, repeat the same edit, and require refusal before any carrier read, replacement write, backup copy, or journal mutation. They also prove that the authoritative recovery artifact remains intact.

### 5. Config status verbosity updates runtime behavior atomically

The visual File Operations control now updates `app.file_task_verbose_degrade_notices` immediately after `TonepoetConfig::save` succeeds. This keeps routine narration and degraded-operation notices on the same persisted `status_verbosity` setting, matching the existing `:file-notices` command.

`advanced_file_operation_control_persists_all_values_atomically` now requires the runtime mirror to become verbose. `advanced_file_operation_control_rolls_back_on_save_failure` requires the mirror to remain unchanged when persistence fails.

### 6. Acceptance evidence is bound to the exact tested source state

The engineering report remains an immutable source artifact covered by `POSTIMAGE_SHA256SUMS`; measured results are generated outside the overlay. Before any validation path that may fail, `RUN_ACCEPTANCE.sh` removes prior checksum authority and writes `RUN_INCOMPLETE`. It then requires:

- `HEAD` to equal the exact `83fe80e` baseline commit;
- every delivered postimage to match;
- the complete tracked working-tree diff to equal `changes.patch` byte-for-byte;
- no unrelated tracked modification;
- no unexpected untracked or ignored path outside the active Cargo target directory;
- the complete tracked source tree plus the overlay-new report to retain one stable manifest digest.

The same source-authority check runs after every executable gate, after machine-record validation, and immediately before certification. A successful run emits:

- `perf-restoration-acceptance-results.md`, including the `POSTIMAGE_SHA256SUMS`, delivered patch, complete overlay-artifact manifest, acceptance-runner, exact tested diff, and complete tested-tree digests;
- `TESTED_OVERLAY.diff` and `TESTED_SOURCE_STATE.manifest`;
- `SOURCE_AUTHORITY.txt`, recording the exact baseline, HEAD, and authority digests;
- `ACCEPTANCE_SHA256SUMS`, covering all logs, all authority records, the delivered manifest and patch, and every tested source file.

`RUN_ACCEPTANCE.sh --verify /path/to/repository` rechecks the immutable overlay artifacts, generated checksum authority, and live source state. The checksum manifest names every tested source file and every artifact listed by `ARTIFACT_SHA256SUMS`, including `RUN_ACCEPTANCE.sh`; a source or runner change therefore invalidates verification rather than leaving apparently valid stale certification. Evidence is required to live outside the repository so generated files cannot enter the tested source state.

### 7. Required mount classes are enforced

`RUN_ACCEPTANCE.sh` now requires Linux, verifies the strict path is actually on `ext4`, resolves and prints mount ID/source/target/filesystem/major:minor/device for both paths, and refuses identical mount IDs. The reduced case must be a distinct non-ext4 mount.

The Rust harness independently requires `TONEPOET_PERF_EXPECT_POLICY` and asserts the detected Tonepoet policy before creating the fixture:

- ext4 case: `FilesystemIdentityPolicy::Strict`;
- reduced case: `FilesystemIdentityPolicy::ContentVerifiedPortable`.

A caller cannot point both variables at one ordinary directory or silently benchmark a reduced case that Tonepoet classifies as strict.

### 8. Cross-platform durability, identity, and Windows protection

The generic metadata end-to-end test is platform-aware. Unix requires no durability warning on the local fixture. Platforms without parent-directory fsync support require the explicit post-commit warning rather than failing an impossible empty-warning assertion.

For standard single-file copy, the writable staged file handle is synchronized before publication. The post-publication standard synchronization step only opens directory roots, avoiding a read-only Windows flush.

Windows journal-free metadata replacement now opens the source and queries both `GetFileInformationByHandle` and `GetFileInformationByHandleEx(FileIdInfo)`. The snapshot retains the 128-bit file ID, 64-bit volume serial, legacy volume/file-index identity, link count, size, creation/write times, and file attributes. Capture refuses link counts above one; precommit validation requires the same one-link file object and version. If the filesystem cannot provide a nonzero stable 128-bit ID, Standard replacement fails closed and directs the user to Strong verification. Platforms that are neither Unix nor Windows likewise fail closed rather than claiming weak identity equivalence.

The Windows commit primitive is now exclusively `ReplaceFileW` with `dwReplaceFlags = 0`. The unsupported `REPLACEFILE_WRITE_THROUGH` value and the unconditional `MoveFileExW` fallback are removed. Failure codes 1175, 1176, and 1177 are classified according to their documented namespace states. The recovery-sensitive 1176/1177 cases retain the replacement carrier under its temporary name and return explicit manual-recovery instructions; no second namespace mutation is attempted. Other errors fail without fallback and remove the uncommitted temporary replacement. Direct Win32 paths are canonicalized and passed in verbatim `\\?\` form, including the `\\?\UNC\` form for network paths.

### 9. End-to-end standard generic metadata coverage

Two always-on tests execute the complete production dispatch beginning at:

`write_metadata_field_transactional_with_control_at_verification(..., Standard)`

The normal-threshold test:

`standard_generic_metadata_write_is_end_to_end_semantic_and_budget_pinned`

uses a real MP3 fixture and pins the in-memory strategy: exact semantic read-back, one source pass, source bytes equal to the original length, one replacement write, replacement bytes equal to the committed length, one file-sync attempt, one parent-sync attempt, zero backup bytes, zero journal writes, and no retained recovery artifact.

The bounded-memory strategy is covered by three complementary pins:

- `standard_metadata_strategy_selects_bounded_memory_immediately_above_limit` proves that the production selector chooses the in-memory route at exactly 256 MiB and the bounded-memory route at the first byte above it.
- `standard_generic_metadata_bounded_memory_route_is_semantic_and_budget_pinned` uses a thread-local test-only threshold override to drive the same production dispatch through that selected branch without imposing a 256-MiB allocation on every workspace test run.
- `bounded_metadata_copy_keeps_temp_restrictive_until_final_attribute_application` proves that streaming an ordinary-mode source through the already-open temporary handle leaves the temp at mode `0600` until final attributes are deliberately applied.

The end-to-end bounded-memory test pins:

- explicit `BoundedMemory` strategy selection;
- exact semantic read-back through the production editor reader;
- one measured Lofty parse pass plus one full source-to-temp copy pass;
- source-byte totals equal the independently recorded parse bytes plus the exact copied carrier length;
- two replacement writes: a counted stream into the already-open restrictive temporary handle plus the bytes actually written by Lofty through a counted `FileLike` wrapper;
- aggregate replacement bytes equal the independently recorded copy and Lofty-write totals;
- one file-sync attempt and one parent-sync attempt;
- zero backup bytes and zero journal writes;
- no `.tonepoet-bak` or stale journal entry.

The counters are attached to the actual Standard writer and the actual database backup/journal mutation sites under `cfg(test)`. The bounded-memory parser uses a counted `Read + Seek` source, and Lofty serializes through a counted `Read + Write + Seek + Length + Truncate` wrapper, so the evidence records actual bytes transferred rather than inferring a rewrite from final file length. The test override changes only the strategy threshold; it does not replace or mock the writer. A future regression that adds an extra source copy, temp rewrite, backup, journal transition, or synchronization therefore fails the corresponding route-specific test.

The release harness repeats those checks against the actual supplied carrier. Its machine record includes the authoritative Rust threshold, and the acceptance runner derives the expected route from that emitted value instead of duplicating the constant in shell. A real carrier above the threshold therefore executes and certifies the actual over-threshold path; the always-on suite pins both the selector boundary and branch behavior without imposing an enormous fixture on every test run.

### 10. Replay tests now reach their stated gate

The two tests:

- `copy_replay_rejects_changed_source_before_publication`
- `move_replay_rejects_changed_source_before_namespace_mutation`

now capture strong retained proofs and execute with a matching strong policy. A test-only reduced-capability identity policy preserves the same-object/length/timestamp-level gate for the same-length in-place mutation, allowing the tests to reach the strong digest comparison deterministically.

Both tests now require the error to contain `replay source content changed` and explicitly reject `verification authority mismatch`. They can no longer pass as false positives at the authority-level gate.

### 11. Fixture scale is explicit

The ordinary always-on counter tests intentionally use small deterministic fixtures. They pin I/O **shape** and remain fast enough for the workspace suite.

A separate ignored release acceptance test is added:

`acceptance_scale_standard_native_move_and_copy_pin_16_files_128_mib`

It requires a strict local mount, creates exactly 16 files totaling 128 MiB, and pins:

- one stat-only native-move walk and zero content hashing;
- exactly 128 MiB copied;
- zero source/destination hash bytes;
- zero redundant rehash bytes;
- one source traversal;
- no destination verification traversal;
- root-plus-parent synchronization, never per-file synchronization.

This report distinguishes three different evidence classes:

1. **Always-on structural counter pinning** — small deterministic fixtures.
2. **Acceptance-scale counter execution** — 16 files / 128 MiB, ignored and run explicitly in release mode.
3. **Wall-clock benchmarking** — real ext4 and reduced-capability mounts through `live_mount_perf`.

### 12. Baseline discrepancy

Resolved as described above: `83fe80e` is the delivery/apply baseline; `839baab` is the earlier feature-lineage reference. Exact preimages remain the byte-level authority.

## Verification architecture

### Explicit authority level

`VerificationMode::{Standard, Strong}` is serialized as lowercase and carried by:

- `FileOperationPolicy`;
- source and destination manifests;
- retained root proofs and undo/redo entries;
- rename execution and replay;
- copy, move, and copy-then-delete workers;
- removal recovery journals.

Authority equality is checked before digest comparison. A standard proof cannot satisfy a strong gate, and a strong proof is not silently downgraded. Undo/redo uses the authority retained by the original operation rather than the user's current preference.

### Standard mode

Standard mode:

- captures digest-free source manifests;
- validates identities, versions, lengths, kinds, and tree membership;
- computes no SHA-256 during the copy loop;
- builds destination authority from staged identities instead of rereading published content;
- performs identity-only quarantine verification and cleanup on strict, portable, Unix, and Windows routes;
- synchronizes a staged root file before publication or a published root directory, plus the relevant parent boundaries, rather than reopening a published regular file or syncing every file and staged directory;
- keeps no-clobber publication and checked degradation behavior.

### Strong mode

Strong mode retains:

- content digests in source manifests;
- inline SHA-256 during copy;
- post-publication destination content verification;
- content-authorized quarantine and removal verification;
- historical per-file/per-directory synchronization behavior;
- content-authorized replay;
- the journaled full-file generic metadata path.

### Recovery-journal compatibility

Removal journals are versioned with verification authority. Existing v4 records remain readable as strong authority. Startup recovery reconstructs the retained authority level so digest-free standard cleanup is not mistaken for malformed strong evidence.

## Counted acceptance tests

| Criterion | Pinning test / evidence |
|---|---|
| Standard manifests are digest-free; strong manifests retain digests | `standard_manifest_is_digest_free_and_strong_manifest_retains_content_authority` |
| Mixed authority is rejected before digest comparison | `mixed_manifest_authority_is_rejected_before_digest_comparison` |
| Native move has one stat-only walk and zero content reads | `same_filesystem_native_rename_has_one_stat_only_walk_and_zero_content_reads` |
| Standard copy has one payload read, zero hash bytes, and root-scoped syncs | `standard_copy_performs_one_payload_read_without_content_verification_passes` |
| Standard portable copy-then-delete has zero verification content reads | `standard_copy_then_delete_remains_zero_content_read_on_portable_mounts` |
| Acceptance-scale strict-mount copy/move uses 16 files and 128 MiB | `acceptance_scale_standard_native_move_and_copy_pin_16_files_128_mib` (ignored release gate) |
| Standard directory rename/replay remains digest-free | `standard_directory_rename_and_replay_proofs_remain_digest_free` |
| Replay follows retained authority and rejects mixed history | `replay_policy_uses_retained_authority_and_rejects_mixed_history` |
| Matching strong replay reaches changed-content comparison | `copy_replay_rejects_changed_source_before_publication`; `move_replay_rejects_changed_source_before_namespace_mutation` |
| Standard recovery reconstructs identity authority | `standard_recovery_journal_reconstructs_identity_authority` |
| Legacy v4 recovery remains strong-compatible | `legacy_v4_recovery_journal_is_parsed_as_strong_authority` |
| Metadata counter wrappers measure transferred bytes rather than file length | `metadata_io_wrappers_count_transferred_bytes_not_final_lengths` |
| Standard generic metadata in-memory semantics and I/O/recovery budget | `standard_generic_metadata_write_is_end_to_end_semantic_and_budget_pinned` |
| Standard generic metadata bounded-memory semantics and I/O/recovery budget | `standard_generic_metadata_bounded_memory_route_is_semantic_and_budget_pinned` |
| Bounded metadata copy retains restrictive temporary permissions | `bounded_metadata_copy_keeps_temp_restrictive_until_final_attribute_application` |
| Windows replacement failure codes map to documented namespace classes | `windows_replace_failure_codes_follow_documented_namespace_classes` |
| Windows hardlinks and file-object replacement are rejected | `windows_standard_metadata_snapshot_refuses_hardlinks_and_detects_replacement`; `windows_standard_metadata_production_route_refuses_hardlinks` |
| Windows commit preserves owner/group/DACL, creation time, and replacement identity | `windows_standard_metadata_commit_preserves_security_and_replacement_identity` |
| Config visual control opens, persists, cancels, and rolls back on persistence failure | the four advanced-control tests listed above |

## Generic metadata writes

For generic Lofty-backed formats only, standard mode:

1. captures source identity/version and owner/mode/timestamp attributes;
2. refuses symlinks and multiply linked files on Unix and Windows;
3. fails closed on filesystems/platforms that cannot supply the required Standard identity authority;
4. performs a read-only refusal check for an armed SQLite recovery entry or stale `.tonepoet-bak` before any successful no-op return;
5. prepares the complete change set before mutation;
6. creates a restrictive same-directory temporary file and keeps it restrictive throughout preparation;
7. for files up to 256 MiB, parses from one in-memory carrier and serializes to that carrier before one temp write;
8. above the guard, streams the source bytes through the already-open restrictive temp handle, then rewrites that handle in place, accounting every parser/copy/Lofty byte at the I/O boundary;
9. restores final owner, group, mode, and timestamps only after the rewrite is complete;
10. synchronizes the temp, revalidates the same source object and version immediately before commit, atomically replaces it, and synchronizes the parent with an honest post-commit warning if durability confirmation fails.

Windows identity uses `GetFileInformationByHandleEx(FileIdInfo)` plus handle link/version information, then commits exclusively through metadata-preserving `ReplaceFileW` with supported flags set to zero. It never falls back to `MoveFileExW`. Documented partial-failure states retain recoverable replacement material where required and return explicit namespace-state diagnostics. Native FLAC and DSF remain exempt and unchanged. Strong generic writes retain the existing database journal and full-file rollback copy.

## Persisted behavior and UI

`[file_operations]` contains:

```toml
verification = "standard"
status_verbosity = "quiet"
auto_close_progress = false
```

Routine success/progress narration is quiet by default, while errors, warnings, skips, partial outcomes, and degraded/durability conditions remain visible. `:messages` retention is unchanged.

Copy, move, undo, and redo progress dialogs expose `Close when done`. Auto-close runs only after the authoritative report has been retained and only for a clean finished report with no errors, warning dispositions, skipped roots, or unattempted roots.

## Editing cursor derivation

The editing cursor is a derived theme element based on warm accent slot 12. It never falls back to the `info`/cyan slot. The deterministic lightness-step algorithm enforces:

- cursor surface versus `input_focused_bg` at least 3.0:1;
- glyph foreground versus cursor surface at least 4.5:1.

The palette-wide regression test is `editing_cursor_clears_contrast_thresholds_in_every_builtin_theme`.

## Performance measurement gate

### Supplied before measurements

Release build, 2026-07-26:

| Mount | Tree | Plain copy | Engine copy | Engine same-mount move |
|---|---:|---:|---:|---:|
| ext4 / NVMe | 192 MiB / 24 files | 110 ms | 3.59 s (32.5x) | 1.36 s |
| fuse.sshfs | 32 MiB / 8 files | 683 ms | 3.96 s (5.8x) | 457 ms |

### Required generated after matrix

The following cells are mandatory and remain unmeasured in this environment. They are populated in the generated `perf-restoration-acceptance-results.md`, not by editing this immutable report:

| Mount | Mode | Plain copy | Engine copy | Ratio | Same-mount move | Warnings |
|---|---|---:|---:|---:|---:|---:|
| ext4 | standard | pending | pending | pending; must be <=1.5x | pending | pending |
| ext4 | strong | pending | pending | pending | pending | pending |
| reduced-capability mount | standard | pending | pending | pending | pending | pending |
| reduced-capability mount | strong | pending | pending | pending | pending | pending |

Metadata edit evidence is also mandatory. Any generic Lofty carrier of at least 50 MiB is valid; the acceptance contract follows the selected strategy rather than imposing an undocumented maximum:

| Carrier | Mode / strategy | Elapsed | Source passes and bytes | Replacement passes and bytes | Syncs | Backup bytes | Journal writes | Read-back |
|---|---|---:|---|---|---:|---:|---:|---|
| generic 50-256 MiB | standard / in-memory | pending | 1 pass; exactly 1x original size | 1 pass; exactly committed size | <=2 | 0 | 0 | exact |
| generic >256 MiB | standard / bounded-memory | pending | 2 passes; actual parser bytes + exactly 1x copied size | 2 passes; exactly 1x copied size + actual Lofty write bytes | <=2 | 0 | 0 | exact |
| generic >=50 MiB | strong / not-applicable | pending | reported | reported | reported | >=1x original size | >0 | exact |

Run the fail-closed acceptance driver from outside the complete repository:

```bash
export TONEPOET_PERF_EXT4_DIR=/writable/path/on/ext4
export TONEPOET_PERF_REDUCED_DIR=/writable/path/on/a-reduced-capability-mount
export TONEPOET_METADATA_PERF_FILE=/path/to/generic-format-file-at-least-50MiB
./RUN_ACCEPTANCE.sh /absolute/path/to/tonepoet
```

The driver requires its evidence directory to be outside the repository, verifies the complete overlay artifact manifest before applying anything, requires distinct mount identities, requires the first filesystem to be ext4, and requires Tonepoet to classify the cases as `Strict` and `ContentVerifiedPortable`, respectively. It executes formatting, Clippy, the complete workspace suite, the 16-file/128-MiB counter fixture, both verification modes on both mounts, and both metadata modes. It re-verifies the exact complete source state after every gate and binds the final evidence to that state.

Do not edit this report or any source-tree postimage after acceptance. Do not declare handoff readiness unless `RUN_ACCEPTANCE.sh` succeeds, the generated results contain all four mount/mode rows plus both metadata rows, and `RUN_ACCEPTANCE.sh --verify /absolute/path/to/tonepoet` succeeds.

## Validation performed in this environment

- Verified the supplied source archive and its original SHA-256 manifest.
- Preserved exact preimage authority for every changed pre-existing file.
- Corrected the native-move counter assertion to one stat-only traversal.
- Audited replay test authority and tightened assertions to the digest-level error.
- Added direct test accounting at actual metadata backup/journal mutation sites.
- Added production-route in-memory and forced bounded-memory metadata semantic/budget tests with platform-aware durability assertions.
- Made the live metadata contract strategy-aware above the 256-MiB memory guard and counted both the restrictive temp stream and Lofty temp rewrite as distinct replacement passes.
- Replaced path-level bounded copies with counted streaming through the existing restrictive temp handle and added a mode-retention regression test.
- Added Windows 128-bit file identity, link-count refusal, same-object precommit validation, and fail-closed behavior where strong identity is unavailable.
- Replaced unsupported `ReplaceFileW` flags and the unsafe `MoveFileExW` fallback with a single metadata-preserving commit, documented error classification, recovery-carrier retention for 1176/1177, and verbatim long-path conversion.
- Added Windows production hardlink refusal and actual commit tests for DACL/security preservation, creation-time preservation, and replacement identity.
- Bound acceptance evidence to the exact complete tested tree and overlay diff, with source checksums and a read-only `--verify` mode.
- Added semantic-no-op recovery-authority refusal tests for both prepared journals and stale legacy backups.
- Added Config-screen advanced file-operation state, rendering, persistence, runtime-mirror parity, and tests.
- Added the 16-file / 128-MiB acceptance-scale counter fixture.
- Strengthened both release performance harnesses as described above.
- Added fail-closed ext4/reduced mount identity and Tonepoet-policy enforcement.
- Separated generated acceptance evidence from immutable source postimages.
- Moved standard single-file synchronization to the writable staged handle before publication.
- Resolved baseline wording in both governing documents.
- Ran `git diff --check`.
- Performed lexical delimiter and changed-call-site audits across all modified Rust files.
- Confirmed version remains 0.4.4 and no function-key or Ctrl+Q binding was added.
- Replayed patch application against a clean copy of the exact supplied preimages and verified postimages during packaging.
- Exercised the acceptance runner with controlled synthetic command output to verify source-authority rechecks, stale-certification invalidation, atomic result replacement, and source-bound evidence verification. This control-flow test is not performance evidence and is not represented as such.

## Repository-level gates not executable here

The following remain mandatory:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Then run the release gates in the previous section. The supplied baseline expectation is 5,162 passing tests across 56 targets. This report does not claim a post-change pass count because the complete workspace and Rust toolchain were not supplied.

## Disclosed residuals

- Standard undo authority is identity/version/tree-membership level, not content equality.
- Reduced-capability filesystems expose weaker identity tokens; standard mode deliberately does not compensate with content rereads.
- The final source revalidation and atomic replace remain separate operations, leaving a narrow hostile-mutator race.
- Unix temp-plus-rename generic metadata writes do not preserve xattrs or ACLs; owner/group/mode/timestamps are preserved. Windows uses `ReplaceFileW` specifically to preserve the destination DACL and other documented Windows file properties.
- Windows Standard replacement requires a nonzero 128-bit file ID and exact link-count authority; filesystems that cannot provide it are refused and must use Strong verification.
- `ReplaceFileW` can report recovery-sensitive partial namespace states. The implementation never retries with a weaker primitive; it retains the replacement carrier for documented 1176/1177 states and requires manual recovery.
- Other non-Unix platforms fail closed for Standard journal-free replacement until a sound identity/link-count implementation exists.
- Platforms without parent-directory fsync support report an honest post-commit durability warning for generic metadata replacement; they do not claim directory-entry durability.
- The in-memory metadata route is limited to 256 MiB and may transiently hold approximately twice the carrier size. Larger carriers use the route-tested bounded-memory two-read/two-write floor instead.
- Standard durability is root-plus-parent rather than per-file/per-directory.
- Checked-best-effort rename fallbacks retain their disclosed concurrent-creation window where native no-replace is unavailable.

## Application protocol

1. Confirm the full repository is the intended `hardening @ 83fe80e` baseline and that `839baab` is its ancestor.
2. Verify every target file against `PREIMAGE_SHA256SUMS`.
3. Apply `changes.patch`, or copy the nested overlay files at identical relative paths.
4. Verify every modified/new file against `POSTIMAGE_SHA256SUMS`.
5. Run formatting, Clippy, the workspace suite, the acceptance-scale counter test, both mount benchmarks, and the metadata benchmark.
6. Verify the atomically generated `perf-restoration-acceptance-results.md` contains all four file-operation rows and both metadata rows.
7. Run `RUN_ACCEPTANCE.sh --verify /absolute/path/to/tonepoet`; this checks `ACCEPTANCE_SHA256SUMS`, every tested source file, the exact delivered diff, and the live postimages.
8. Do not hand off if any preimage differs, any gate fails, the standard strict-mount copy ratio exceeds 1.5x, the generated evidence is incomplete, source verification fails, or strong-mode behavior regresses.
