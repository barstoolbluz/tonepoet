# Required checks before merging

Run these in a Rust-equipped environment from the crate root:

```bash
cargo fmt --check
cargo check --all-targets --all-features
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

This generated bundle was statically audited in the ChatGPT sandbox, but the sandbox did not include `cargo` or `rustc`, so the compiler-backed checks above were not executed here.
