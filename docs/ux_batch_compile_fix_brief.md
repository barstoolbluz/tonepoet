# UX Batch: 9 Compile Errors to Fix

The UX improvements bundle was applied. Two `ConversionOptions` constructors in `wizard_integration.rs` were already fixed (added `force_encode: false, create_disc_subfolders: false`). 9 errors remain — all are function argument count mismatches from changed signatures that weren't propagated to all call sites.

## Remaining Errors

```
7x error[E0061]: this function takes 6 arguments but 5 arguments were supplied
   → src/tui/app.rs:1403, 1469, 1561, 1580
   → src/tui/keybindings.rs:10440, 10552, 22714

1x error[E0061]: this function takes 7 arguments but 6 arguments were supplied
   → src/tui/event_loop.rs:1311

1x error[E0308]: `match` arms have incompatible types
   → src/tui/keybindings.rs:7411
```

The full `cargo check` output is in `docs/cargo_check_output.txt`.

## Your Task

Fix the 9 remaining compile errors. These are mechanical — add the missing arguments to match the updated function signatures. Do not revert any UX changes.
