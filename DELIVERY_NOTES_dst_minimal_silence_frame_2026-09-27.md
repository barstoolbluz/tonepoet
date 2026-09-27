# Delivery notes: minimal DST silence frames (issue #50)

Date: 2026-09-27
Base archive: `tonepoet_dst_minimal_silence_frame_bundle_2026-09-27.tar.gz`
Base archive SHA-256: `b7daff3f2bd7bcbc887562a9349607a0ba058fd012fe0a95eb366bcae1951f5e`
Underlying code: `main` @ `bdde58d` / bundle snapshot `1bd3d1c` (v0.5.3), as recorded by `SNAPSHOT_README.md`.

## Verified diagnosis

The reported failure is real, but the reference behavior is narrower than “pad short frames with zeros.”

The public `Sound-Linux-More/sacd-extract` MPEG-4 DST decoder at commit
`9b24d6e9d77ec728576be11020db032f30e7ddd0` does two distinct things:

1. `UnpackDSTframe` parses segmentation, mapping, filter, and probability-table syntax from the physical frame and treats EOF there as an error.
2. Once syntax parsing is complete, it records only the physically remaining arithmetic-code bits. The arithmetic decoder consumes those bits and supplies zeros after that arithmetic tail is exhausted. If the arithmetic tail is non-empty, its first physical bit must be zero; a completely empty tail is legal.

That is the contract implemented here. No general short-frame zero filling was added.

For the reported stereo frame `ff 02 00 c0 41 80 05 6e`, compressed syntax consumes 54 of 64 physical bits. Ten arithmetic bits remain: the zero marker plus nine initialization bits. The decoder needs three implicit zero bits to complete its 12-bit arithmetic initialization. The old implementation enabled zero-extension only after trying to read all 12 initialization bits, so it failed at EOF.

The reported 21-byte multichannel frame parses specifically as five-channel DST. Its compressed syntax consumes 155 of 168 bits, leaving exactly 13 arithmetic bits: the marker plus all 12 initialization bits. That explains why it already decoded before this fix.

## Correction

The production change is limited to the compressed DST arithmetic phase boundary:

- `BitReader` can now report whether any physical frame bit remains and exposes a one-way arithmetic-tail zero-padding transition.
- `DstDecoder::decode_compressed_dst_payload` still completes `CompressedSyntax::read` under strict EOF semantics.
- The leading arithmetic zero marker is validated only when a physical arithmetic bit remains, matching the reference decoder's `ADataLen > 0` check.
- `ArithmeticCoder::new` enables zero-extension before loading its 12-bit initial code value, matching reference arithmetic initialization and subsequent renormalization.
- The raw/uncompressed DST path is unchanged.

This adds one constant-time physical-bit check per compressed frame and no allocation, copying, locking, or hot-loop work.

## Fixtures and regression coverage

Added the two issue frames as source-controlled decoder fixtures with literal expected DSD output:

- `issue50_minimal_stereo.dst.bin`: 8 bytes, SHA-256 `ffe702794e29482dcd1037a96341c2fe4fc593f856142065af269fec0c2480f9`
  - expected: 9,408 bytes of `0x99`
  - expected SHA-256: `4faaab209cd51206464485ef0d28513db6ad0494897a8cee6a4b27eac0ce8e58`
- `issue50_minimal_5ch.dst.bin`: 21 bytes, SHA-256 `c7fedcb5a51dd6eb39c323d9324e3956138a71f3911df65a62e5b470ffe51424`
  - expected: 23,520 bytes of `0x99`
  - expected SHA-256: `6fb556828667ebb8641c460c270e16dbeae01ee864959710cc2c4f35c0c8473c`

`ISSUE50_PROVENANCE.json` records the fixture origin, expected-output authority, and reference-decoder semantics. `SHA256SUMS` now pins all four new files.

Regression tests now cover:

- byte-exact decoding of both reported frames;
- compressed frames with no explicit arithmetic payload for every decoder channel count 1 through 6, including the four-channel case whose syntax ends exactly at the physical end of the frame;
- strict rejection of a stereo frame truncated inside compressed syntax;
- rejection of a physically present nonzero arithmetic marker;
- strict `ExtractIntegrityOptions::strict()` extraction of the reported stereo and five-channel frames with no integrity loss reported and byte-exact `0x99` DFF audio payloads.

The pre-existing pinned compressed fixtures retain large physical arithmetic tails (at least 23,513 bits after syntax in the stereo corpus and 67,974 bits in the six-channel corpus), so this phase-boundary correction does not change which bits they consume. Their existing SHA-256 fixture set was revalidated unchanged.

## Reference-path qualification boundary

The exact Bach disc image is not present in this bundle, so a whole-disc/whole-album Reference conversion cannot be rerun in this runner. The regression test exercises the exact failing encoded frame through the strict ISO extraction path and asserts a clean integrity report. The full-disc conversion remains the final operator acceptance check against the physical source.

No Reference source lock changed. The following lock hashes are unchanged from the base bundle:

- `flake.lock`: `7a306a3191321cccf66e6008c0a076cdf2c2d72529c9f5d5917ac38b2f7cf582`
- `reference_source_lock.rs`: `8b4e293b136f0afb3fb36d98192c39dd60c6184c5513d1982155e9c15acd2131`
- `tests/reference_source_lock.rs`: `a04f4dcc3a0b26537206b01c5af5ad47de1e9f16cafacba660bec42966da2906`
- root `Cargo.lock`: `6e66aa5f3237559cfafda4e97d2e96b90b212ada8e4bbeb9cdbc621299091c8a`
- `crates/tonepoet-backend/Cargo.lock`: `ab6ab20d92219393764319265e5b72f22acce0be0c4d829c034f11be53eb206d`

## Changed files relative to the supplied bundle

- `crates/sacd-rs/src/dst/bitreader.rs`
- `crates/sacd-rs/src/dst/decoder.rs`
- `crates/sacd-rs/src/extract.rs`
- `crates/sacd-rs/src/dst/fixtures/SHA256SUMS`
- `crates/sacd-rs/src/dst/fixtures/ISSUE50_PROVENANCE.json`
- `crates/sacd-rs/src/dst/fixtures/issue50_minimal_stereo.dst.bin`
- `crates/sacd-rs/src/dst/fixtures/issue50_minimal_stereo.dsd.bin`
- `crates/sacd-rs/src/dst/fixtures/issue50_minimal_5ch.dst.bin`
- `crates/sacd-rs/src/dst/fixtures/issue50_minimal_5ch.dsd.bin`
- `DELIVERY_NOTES_dst_minimal_silence_frame_2026-09-27.md`

## Validation performed in this runner

This sandbox does not contain `nix`, `flox`, `cargo`, `rustc`, or `rustfmt`, so this delivery does not claim a Rust compile, format, clippy, or workspace-test pass here.

Validation performed locally:

- verified the reference decoder's strict-syntax / zero-extended-arithmetic boundary from `unpack_dst.c`, `dst_data.c`, and `dst_fram.c` at the pinned public commit above;
- independently parsed the reported frames and confirmed their 54/10-bit stereo and 155/13-bit five-channel syntax/arithmetic split;
- independently decoded both reported frames with the reference algorithm model to the expected constant `0x99` payload and the pinned expected hashes above;
- independently exercised zero-tailed complete compressed syntax for every channel count 1 through 6;
- verified all existing and new entries in `crates/sacd-rs/src/dst/fixtures/SHA256SUMS`;
- validated `ISSUE50_PROVENANCE.json` as JSON;
- performed delimiter-balance scans on all changed Rust sources;
- `git diff --no-index --check` against a freshly unpacked copy of the supplied base reported no whitespace errors;
- verified all Reference and Cargo lock hashes above remained unchanged.
- round-trip extracted the final deterministic delivery archive and byte-compared it with the audited working tree.

Required handoff gate in the repository's pinned development environment:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --check
cargo check -p sacd-rs
cargo test -p sacd-rs --no-fail-fast
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

Then rerun the reported Bach stereo Reference conversion and verify a clean integrity report for the complete album.
