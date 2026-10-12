# R34 engineering checkpoint and handoff — 2026-10-12

Status: **SOURCE CORRECTIONS PACKAGED; BUILD-HOST ACCEPTANCE OPEN.**

Input archive: `TonePoet_LOGGING_R34_five_regressions_2026-10-12.tar.gz`
Input SHA-256: `440fd00ab05564fbfa3ed3528c64a5c5908ce39d531a5ca8835682c0581f7c79`
Brief: `BRIEF_LOGGING_R34_five_regressions_2026-10-12.md`
Base tree: pristine extracted R34 input / R33C corrections, with no `.git` in original.
Changes patch: `R34_FROM_R33C_SOURCE.patch`.

## Change map

1. SACD multichannel tab and `:area` selection: `src/tui/keybindings.rs`,
   `build_sacd_editor_state_for_area` and its callers. Select the requested
   area explicitly *without discarding the sibling's original TOC*. The
   stereo count defines the MCH metabase ID offset. The read projection
   additionally falls back to the existing XML area split if sidecar and TOC
   row counts disagree. Write-path tag-transfer validation stays strict.
2. SACD single-area MCH: in the legacy sidecar save path, treat XML IDs 1..N
   as the sole MCH area when there is no stereo TOC and no independently
   classified MCH XML area. Do not mirror that edit onto itself. Includes a
   regression exercising an XML rewrite from a single-area MCH editor.
3. Ordered PERFORMER list: `cue_album_user_metadata_for_track_indices`
   distinguishes an existing shared album-level performer from track-level
   credit rows. A shared album list is preserved in the inert CUE album
   metadata, while different one-file-per-track performers remain independent
   per-track credits. Uses case-insensitive detection of pre-existing inert
   track PERFORMER metadata.
4. R33C authority test: its original two-tracks-per-FLAC-image sidecar CUEs
   cannot acquire `IndividualFiles` authority. Keep the first half unchanged,
   then reauthor each CUE as one track per FLAC before testing file-authoritative
   DISCNUMBER. Continue asserting that native authority leaves both CUEs
   byte-identical. No authority resolver exception is introduced.
5. Transfer filter: change the stale expected extension set to include `.iso`
   *only for the transfer picker*, asserting global audio detection still
   excludes `.iso`.
6. Existing R33 input `src/tui/browse.rs` already has the `SacdIso` match arm
   mentioned in the brief. No duplicate change was needed.

Only the two Rust sources (`src/tui/keybindings.rs` and
`src/tui/tag_interchange.rs`) changed; no DSP, Nix, pipeline, qualification, or
source-locked material was edited. Four additional `r34_` tests cover hybrid
MCH selection, single-area MCH save, album ordered PERFORMER, and distinct
per-file PERFORMER.

## Completed offline verification

- `git diff --check` passed for source changes.
- `bash -n R34_ACCEPTANCE.sh` passed.
- Pygments Rust lexing found no error tokens in changed Rust files; structural
  delimiter balance checked independently. **This is not a Rust parse or
  compilation result.**
- Applied the source patch to an independently extracted pristine baseline with
  `patch -p1 --fuzz=0`; byte-compared both changed files; reversed the patch
  with `patch -R -p1 --fuzz=0` and byte-compared restored originals.
- SHA-256 full-tree manifest and independently extracted archive verification
  are recorded in `R34_STATIC_AUDIT.txt` and `R34_SOURCE_SHA256SUMS.txt`.
- No cargo, rustc, rustfmt, or Nix on this host. `R34_ACCEPTANCE.sh` correctly
  refused to report success without a Cargo toolchain (exit 2).

## Required build-host acceptance — still OPEN

1. Extract the supplied complete archive and verify `R34_SOURCE_SHA256SUMS.txt`
   from the `tonepoet/` root (`sha256sum -c R34_SOURCE_SHA256SUMS.txt`).
2. On a configured Rust 1.93+ and audio build host, with a **real SACD ISO**:
   `R34_CHECK_FORMAT=1 bash ./R34_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso`.
   This begins with a full-workspace `cargo check --locked --workspace --all-targets`,
   runs the five original failing tests plus the new R34 regressions, and then
   chains R33C -> R33 -> R32 -> ... -> Reference requalification and freshness.
3. If formatting gates fail in untouched legacy sources, evaluate those
   failures without indiscriminately reformatting the repository. If code or
   tests change, regenerate the patch, source manifest, and final archive.
4. Confirm against a physical SACD ISO that stereo and MCH selections read,
   edit, save, reopen, and preserve unrelated tracks; run Reference qualification
   without `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE=1`.
5. The reported contention-only scanner flake and previously stale Reference
   source lock remain distinct from R34's five source failures; do not mark
   them fixed without reproducible evidence.

## Resume instructions

Everything needed to resume is inside this source archive: original brief,
corrected source, focused tests, source patch, integrity manifest, acceptance
script, and this checkpoint. Apply the patch only against the exact R34 input
source, **not** against an already-corrected tree. Re-run checksum and compile
acceptance after any correction. No background job or deferred execution is
running.
