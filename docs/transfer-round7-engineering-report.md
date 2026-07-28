# Engineering Report - Transfer Carrier Semantics Round 7, Final Corrective Revision

**Delivery date:** 2026-07-28  
**Governing baseline asserted by the supplied documents:** `hardening` at `02b8822`  
**Required application version:** `0.4.4`  
**Modified files:** 13  
**Received source archive SHA-256:** `7a950b3cb759287ab2bf51494281731f4a5a68ee6bded8c413871b97b8ec6bee`

## Disposition

This revision treats all three findings from the final corrective review as
handoff blockers. It removes unbounded ordinary-file traversal from transfer
admission, makes installer-journal publication recoverable from every setup
step, and moves CUE version/geometry comparison inside the mutation authority
so the writer validates the exact snapshot it is about to use.

The revision also retains the preceding corrective work: canonical Audio plus
CUE composition, explicit single-image/CUESHEET collapse authorization,
carrier-specific no-op reporting, operation-scoped classification caches, and
track-number-safe CUE rewriting.

The Rust code still requires formatting, compilation, clippy if required by the
repository, and `cargo test --workspace` in the complete repository. The
supplied archive is not a complete workspace and this environment has no Rust
toolchain. This report therefore does not claim that the repository-level
handoff gate has passed.

## Final corrective findings and resolutions

### 1. Transfer admission is structural; ordinary fallback is bounded exactly once

The shared editor admission path still supports its established ordinary-file
expansion behavior. Transfer classification now enters a separate
`StructuralOnly` admission mode. That mode discovers and admits same-folder CUE
surfaces but never populates `ordinary_paths`, including when:

- the selected directory has no CUE candidate;
- every CUE candidate is rejected; or
- an admitted member is ordinary rather than a synthetic single-image surface.

Only after structural admission has failed to select a CUE carrier does transfer
classification invoke `expand_audio_paths_for_transfer_limited`. The transfer
wrapper uses the existing deterministic bounded traversal engine, the existing
production caps, and cooperative cancellation. Thus a cue-less or rejected-CUE
directory cannot first undergo an unbounded walk and then a bounded second walk.

The tests inject deliberately small limits and prove both the visitation cap
and audio-file cap for cue-less and rejected-CUE directories. A separate test
proves cancellation interrupts the bounded traversal.

Named pins:

- `transfer_structural_admission_never_performs_ordinary_directory_expansion`
- `transfer_directory_fallback_enforces_caps_for_cueless_and_rejected_cues`
- `transfer_directory_fallback_observes_cancellation_during_bounded_walk`

### 2. Installer setup journal is unpublished until complete and durable

The installer now constructs setup state under:

```text
.tonepoet-round7-transaction-building.<id>
```

The exit trap is installed before the first directory is created. While the
journal is unpublished, no target mutation is reachable. The installer then:

1. creates and fsyncs the unpublished directory;
2. creates its backup directory;
3. copies and fsyncs both exact manifests;
4. writes and fsyncs the initial `BUILDING` state;
5. fsyncs the complete unpublished journal;
6. atomically renames it to `.tonepoet-round7-transaction.<id>`; and
7. fsyncs the target root before preparing backups or replacements.

Recovery discovers both published and unpublished names. An unpublished setup
journal is removed only after proving that every governed target still matches
the exact bundled preimage. This is safe because target mutation cannot begin
before publication. A normal invocation and `--recover` both clean such setup
journals automatically; `--check` reports the pending recovery without
mutating it.

Fault injection covers every setup boundary: 11 forced SIGKILL points and the
same 11 ordinary-failure points. Every case recovered to all 13 exact preimages,
left no transaction directory, and subsequently passed `--check`.

### 3. CUE validation is bound inside the mutator

#### Sidecar CUE

`rewrite_cue_sidecar_metadata_from_cuesheet_validated` reads the carrier bytes
itself and passes that exact decoded snapshot to the transfer validator. The
validator checks:

- the exact resolved image identity;
- the complete track-number set; and
- every sorted `INDEX 01` frame position.

The structured rewrite is composed from those same bytes. Immediately before
its atomic same-directory replacement, the mutator rereads and compares the
complete original byte sequence. It refuses rather than overwriting if the
path changed. The unchanged/no-op path performs the same final comparison.
Validation is therefore no longer a caller-side read followed by an unrelated
writer-side reread.

A regression pin mutates the CUE after the mutator's snapshot validator runs
and proves the concurrent content is retained and the planned rewrite is
refused.

Named pins:

- `sidecar_write_re_admission_detects_target_identity_and_complete_track_geometry`
- `validated_sidecar_rewrite_refuses_a_change_after_snapshot_validation`

#### Embedded FLAC CUESHEET

The transfer route no longer passes a previously observed CUESHEET into the
generic classified writer. It enters a dedicated native FLAC compare-and-write
path. After acquiring the native common write claim and completing journal
recovery and hard-link checks, that path reads the FLAC metadata snapshot,
requires exactly one CUESHEET comment whose value exactly equals the observed
value, builds the replacement from that same metadata object, and only then
commits through the existing crash-recoverable native writer.

A stale observed CUESHEET is refused byte-for-byte without mutation.

Named pin:

- `embedded_cuesheet_transfer_compare_and_write_refuses_stale_observation`

## Earlier corrective findings retained

### Canonical Audio plus CUE filter composition

`src/convert/classify.rs` contains one declarative extension-to-format table.
It generates both the application classifier and
`SUPPORTED_AUDIO_FILE_EXTENSIONS`. The transfer picker composes that canonical
coverage with only `cue`; the reusable global `FilePickerFilter::Audio` remains
unchanged. The exhaustive transfer-filter pin compares complete set equality,
not representative examples.

Named pins:

- `classify_file_maps_supported_audio_extensions_case_insensitively`
- `transfer_picker_filter_is_canonical_audio_plus_cue_without_widening_global_audio`

#### Disclosed global classification consolidation

The canonical-table correction intentionally consolidates shared
`EntryKind::AudioFile` classification rather than creating a transfer-only
shadow list. Consequently aliases such as `bwf`, `m4b`, `m4r`, and `caf` are
recognized consistently by browse classification, queue/admission paths,
metadata-editor admission, and transfer enumeration. This is broader than only
changing the two transfer picker filters, but removes the drift that caused the
original defect.

The classifier has exhaustive case-insensitive extension coverage; the
transfer picker has exact canonical-union testing; and existing queue/editor and
non-transfer picker suites remain the repository-level regression authority.
Because the full workspace could not be compiled here, those cross-surface
existing pins still must run in the complete repository, with particular
attention to ambiguous containers such as CAF.

### Explicit single-image/CUESHEET collapse authorization

The ordinary planner entry point is fail-closed and uses
`FirstTrackCollapseEligibility::Forbidden`. Cardinality collapse is available
only when the caller explicitly proves a one-file image surface with a nonempty
CUESHEET. An ordinary single audio file cannot silently receive first-track
values.

Named pins:

- `track_dimension_plans_cover_n_to_n_mismatch_collapse_and_one_file_skip`
- `first_track_collapse_requires_single_image_and_nonempty_cuesheet_evidence`

### Honest CUE rewrite/no-op status

`TagTransferReport` retains the concrete target. CUE status text identifies the
sidecar path or embedded image and distinguishes rewritten, unchanged, and
failed outcomes. Reapplying an identical transfer reports `0 rewritten, 1
unchanged, 0 failed`; it does not claim that fields were written.

Named pins:

- `sidecar_cue_transfer_route_preserves_structure_and_is_idempotent`
- `embedded_flac_cue_transfer_round_trips_and_is_idempotent`

### Operation-scoped batched carrier classification

Transfer classification performs one structural admission pass for the whole
selection, indexes admitted surfaces by canonical parent and image identity,
and performs one merged embedded-tag read. Per-root work is map lookup rather
than repeated discovery and tag reads. An explicit `.cue` selection bypasses
irrelevant folder and embedded-tag discovery.

Named pin:

- `multi_file_transfer_classification_batches_admission_and_embedded_reads_once`

### Sorted planning and declaration-order-safe rewriting

The structured CUE rewriter matches desired tracks to target tracks by authored
track number rather than zipping numeric-plan order against declaration order.
Missing or duplicate numbers are refused. Gapped and non-one-based numbers
remain valid.

Named pin:

- `metadata_rewriter_matches_tracks_by_number_when_declaration_order_differs`

## Retained Round-7 implementation

The overlay retains:

- deterministic marked-file completion with directory filtering and visible
  sorted order;
- compatibility through legacy `path` plus transfer-only `paths`;
- priority placement and contextual labels for picker confirmation;
- policy threading at the resolution layer rather than admission;
- consistent carrier resolution for directory, CUE, and image gestures;
- matched sidecar/embedded read semantics;
- track-dimensional CUE rows and field capping;
- target-template CUE composition through the structured sidecar engine;
- FLAC-only embedded CUE writes with non-FLAC fail-closed behavior; and
- blocking browse-side write confirmation over a frozen prepared snapshot.

## Overlay provenance

`PROVENANCE/` includes:

- the exact received `transfer_round7_bundle.tar.gz`;
- the separately supplied governing brief; and
- the separately supplied handoff readme.

`verify-overlay.sh` checks hashes, rejects unsafe/non-regular archive members,
extracts into a private temporary directory, verifies all 27 source-archive
manifest entries, and proves the separately supplied governing documents are
byte-identical to the archived copies.

This independently exposes the received preimages. It does not cryptographically
prove that the archive came from Git commit `02b8822`; no `.git` object database
or signed commit evidence was supplied.

## Installer transaction semantics

Beyond unpublished setup publication, `apply-overlay.sh`:

- locks the target directory inode without a persistent lock file;
- validates exact all-preimage or all-postimage state;
- refuses symlinks, non-regular files, divergent/mixed states, and hard-linked
  targets;
- copies attributes separately from content and reads source content with
  `O_NOATIME` through GNU `dd iflag=noatime`;
- preserves mode, owner/group, atime/mtime, ACLs, xattrs, and security labels
  where supported, failing if metadata copying fails;
- fsyncs manifests, backups, stages, journal state changes, replaced files, and
  destination directories;
- advances through durable `BUILDING`, `PREPARED`, `APPLYING`, and `COMMITTED`
  states;
- uses a same-directory atomic rename for each file;
- restores exact preimages from durable backups when an `APPLYING` transaction
  is recovered; and
- makes recovery restartable if another crash leaves a partial restore file.

The operation is crash-recoverable but cannot make 13 pathname replacements
globally atomic to concurrent readers. A reader may briefly observe a mixed
old/new set during the apply phase. Replaced files receive new inode numbers and
ctime values. Parent-directory timestamps may change. Hard-linked targets are
refused rather than silently splitting link identity.

## Validation performed

Passed in this environment:

- source-archive path-safety inspection;
- all 27 source-archive internal manifest entries;
- byte equality of separately supplied and archived governing documents;
- exact 13-file preimage/postimage coverage;
- overlay payload/provenance verification;
- Bash syntax checks for both delivery scripts;
- source-diff whitespace scan;
- lexical delimiter-balance scan over all 13 modified Rust files;
- targeted static invariants for structural-only transfer admission and bounded
  fallback ownership;
- exact-preimage `--check`;
- successful 13-file application and exact postimage verification;
- idempotent postimage reapplication;
- preservation of mode, uid, gid, atime, mtime, and a real `user.*` xattr;
- fail-closed hard-link refusal with every preimage unchanged;
- forced SIGKILL after five replacements, pending-recovery detection, and exact
  rollback of all 13 preimages;
- divergent-preimage refusal without collateral mutation;
- 11 setup-phase SIGKILL injections, each followed by automatic unpublished or
  published journal recovery; and
- 11 setup-phase ordinary-failure injections with the same exact-preimage
  result.

Not performed here:

- Rust parsing by `rustc` or `rustfmt`;
- Rust compilation;
- clippy;
- `cargo test --workspace`; or
- empirical ACL and SELinux-label preservation on a filesystem configured with
  those facilities.

The source archive omits most workspace members named by `Cargo.toml`, and this
environment has no Rust toolchain. The complete repository must run its format,
compile, clippy-policy, and workspace-test gates. The governing 5,265-passing
baseline has not been independently re-established here.

## Deliberate scope fences retained

- Embedded CUESHEET writes remain FLAC-only.
- SONGWRITER remains excluded.
- CUE transfer never clears an existing CUE field because absent replacement
  fields leave original lines untouched.
- Native multi-FILE CUE albums remain honest refusals.
- Disc-image transfer, ISRC writeback, range selection, cross-directory marks,
  the future Config cascade, library/album abstraction, Custom builder, and
  Paste tags remain out of scope.
