# Feedback on rich progress reporter patch v2

## Repo state

The patch was applied to `https://github.com/barstoolbluz/tonepoet.git` branch `main` at commit `a8138cc`. The codebase had been run through `cargo fmt` before application (29 files reformatted).

## What succeeded

The Python applicator patched `reporter.rs` and partially patched `processor.rs` and `stages.rs` before failing. The `BroadcastReporter` code in `rust_sections/broadcast_reporter_section.rs` looks correct and well-structured.

## Three issues found

### 1. Multiline formatting mismatch in processor.rs

The applicator searches for this single-line string:

```
let progress = match &final_status { ConversionStatus::Completed { .. } => 100.0, _ => 0.0, };
```

But after `cargo fmt`, the actual code is multiline:

```rust
let progress = match &final_status {
    ConversionStatus::Completed { .. } => 100.0,
    _ => 0.0,
};
```

This caused `error: could not patch final status progress calculation in processor.rs` and stopped the entire application.

**Fix:** Use regex or multiline matching instead of exact single-line string matching. Or better: anchor on a unique nearby comment or function name and use structural insertion rather than literal replacement.

### 2. Missing `progress` field on `AppMessage::ConversionProgress`

The patch adds `progress` to the `AppMessage::ConversionProgress` destructuring in `event_loop.rs`:

```rust
AppMessage::ConversionProgress {
    item_id,
    progress,  // <-- new field
    status,
} => {
```

But it does not add the field to the enum definition in `src/tui/message.rs`:

```rust
// Current (unchanged by patch):
ConversionProgress {
    item_id: String,
    status: crate::convert::ConversionStatus,
},
```

This causes `error[E0026]: variant AppMessage::ConversionProgress does not have a field named progress`.

**Fix:** Add `progress: f32` to the `AppMessage::ConversionProgress` variant in `src/tui/message.rs`. And update all construction sites that create this variant.

### 3. Type mismatch in `run_sevenzip_pipeline_conversion_item`

The patch creates a new function `run_sevenzip_pipeline_conversion_item` with parameter:

```rust
tool_paths: HashMap<String, String>,
```

But the caller passes `HashMap<String, PathBuf>` (which is the type used everywhere else in the codebase — see `ProcessorConfig.tool_paths`).

This causes `error[E0308]: mismatched types`.

**Fix:** Use `HashMap<String, PathBuf>` for the parameter type.

## Recommendation

Please produce a corrected v3 bundle. Suggested approach:

1. Clone the repo at commit `a8138cc` and run `cargo fmt` first, so anchors match the formatted code.
2. Add the `progress: f32` field to `AppMessage::ConversionProgress` in `src/tui/message.rs`.
3. Use `HashMap<String, PathBuf>` for `tool_paths` in the new 7z routing function.
4. Test with `cargo build` and `cargo test --lib` in the nix shell before packaging.

Alternatively, deliver the complete replacement files for the 5-6 files touched, rather than a Python text-replacement script. This avoids all anchor-matching fragility.

## Build commands

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo fmt
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

Current test count: 591 (must not regress).
