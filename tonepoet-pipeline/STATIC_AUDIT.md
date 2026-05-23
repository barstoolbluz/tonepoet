# Static audit for v10 bundle

This sandbox did not include `cargo` or `rustc`, so compiler-backed checks must still be run in a Rust-equipped environment. The following static checks were run before packaging this bundle:

- zip input extracted successfully
- stale-pattern scan for `BuilderPending`, `DitherPolicy`, `EncodeOptions`, `MainConversionOptions`, `MainDitherType`, `write_id3v2`, and `metaflac --list`
- duplicate named-field scan for enum variants
- heuristic public-item doc scan across `src/*.rs`
- manual inspection of the planner, plugin registry, metadata-disposition pruning, FFmpeg/SoX/SSRC/loudgain/metaflac/FLAC command builders, and regression tests

Required compiler-backed checks before merge:

```bash
cargo fmt --check
cargo check --all-targets --all-features
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```
