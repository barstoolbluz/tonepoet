# R35 static and source-scope audit — 2026-10-12

**Status: static checks PASS; compilation, runtime tests, and Reference requalification NOT RUN.**
This environment has `ffmpeg`, but **no Cargo, rustc, rustfmt, or Nix**; network resolution for Rust toolchain installation is unavailable. A lexical check is not a Rust compile.

## Inputs and authority

- Statement of work: `BRIEF_LOGGING_R35_album_view_fixture_2026-10-12.md`.
- Full source input: `TonePoet_LOGGING_R35_album_view_fixture_2026-10-12.tar.gz`.
- Input archive SHA-256: `4d46c48cc264deef1cb066e71c0daa6404facd916d4a45d24abd26cf30dbdcbd`.
- Input `src/tui/keybindings.rs` SHA-256: `1a35468bf1ce4e03dd4012357f03c16c0e38007eb169f88b7a755065c669a652`.
- Corrected `src/tui/keybindings.rs` SHA-256: `37224d4440b5b2123bfe81d1a273ade440cb494ebce8ef89c6bde7b1b5d90a23`.
- `R35_FROM_INPUT.patch` SHA-256: `8bd309df98416ff5f91e5f989312997d714044655da8654c7346f649aa627a17`.

## Provenance and integrity checks performed

1. Compared **every original regular file** (968 files) and the original symbolic link against the supplied tar archive: only `src/tui/keybindings.rs` differs. No production code or other original artifact was changed.
2. Applied `R35_FROM_INPUT.patch` with `patch -p1 --fuzz=0` onto a pristine original copy; resulting Rust source was byte-identical to delivered R35. Reversed the patch with `patch -R -p1 --fuzz=0` and byte-compared to the original: **PASS**.
3. `git diff --no-index --check` returned no whitespace diagnostics: **PASS**.
4. Pygments Rust lexical scan: zero lexer error tokens both before and after correction. Balanced Rust punctuation token delimiters in the complete source before and after: **PASS**. These checks do not detect type errors.
5. Focused source assertions confirm that the original two-track image half still asserts `Album view`; the one-track-per-carrier half now asserts `File tags`, invokes the user-facing Add Key and inline editor paths, asserts native scope, retains the production Save/reopen call, checks both audio mutations and both CUE byte-preservation checks: **PASS (static only)**.
6. `bash -n R35_ACCEPTANCE.sh`: **PASS**. Running its preflight with Cargo absent correctly returns exit status 2 instead of claiming a test pass.

## Why this is a test correction, not a production correction

The first part of the test uses two-track CUEs over each FLAC image; the admitted surfaces are genuine image album parts, so `Album view` and the side-level DISCNUMBER partition are appropriate.

The second part explicitly rewrites both CUEs to **one track for each physical FLAC**. Those surfaces are `MetadataSidecar` representations by design. `metadata_surface_album_groups` keeps them distinct from synthetic image parts, and `open_metadata_editor_for_resolved_groups` does not create a synthetic `Album view` for metadata-sidecar groups. Instead, the configured `IndividualFiles` authority exposes an aggregate `File tags` presentation, with independently selectable CUE tabs. The existing `editor_and_queue_share_one_track_per_file_metadata_sidecar_membership` and `multipart_metadata_sidecars_remain_independent_field_authorities` tests support this boundary.

`File tags` does not invent a blank `DISCNUMBER` field. The R35 test now creates the field through the real Add Key UI handler, writes **explicit positional values 1 and 2** to the native FLACs (rather than applying the Album-view-specific `1; !` implicit partition), calls the existing production Save path, verifies the sidecar CUEs are byte-identical, and checks saved values and clean originals after reopen.

**No production behavior, runtime algorithm, or performance-critical source was modified.**

## Known limitations and prior gates

- **Cannot assert successful build or test run here**: Cargo and Rust compiler are absent.
- The brief reports `7457 passed / 2 failed` at the preceding gate: the isolated R33C assertion and source-lock freshness. Those are **reported upstream results**, not independently repeated here. R35 changes the former test expectation but does not claim to resolve the latter.
- R33 changes to `stages.rs` still owe **fresh source-locked Reference requalification on a real SACD ISO**. No fixture was supplied to this environment; do not bypass with `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE=1`.
- The archived `R34_SOURCE_SHA256SUMS.txt` already mismatched `src/tui/keybindings.rs` **in the supplied R35 input**, owing to its subsequent compile correction. The old manifest is retained as an historical R34 artifact. **Use `R35_SOURCE_SHA256SUMS.txt` for the delivered R35 snapshot**; it covers regular files and excludes itself to avoid a checksum cycle; the archived directory symlink is preserved separately.

## Final build-host acceptance

From the extracted `tonepoet/` root:

```bash
sha256sum -c R35_SOURCE_SHA256SUMS.txt
cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority
bash ./R35_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
```

The chained R34D acceptance incorporates R34C, R34, R33C, older focused suites, and Reference requalification. R35 then runs a full workspace test gate. Review any unrelated scanner contention flakes separately; do not presume they were fixed by this test-only correction.
