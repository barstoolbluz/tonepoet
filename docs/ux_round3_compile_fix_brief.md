# UX Round 3: 9 Compile Errors

Same pattern as previous rounds — function signatures changed but not all call sites updated.

```
7x error[E0061]: takes 6 arguments but 5 supplied
  → src/tui/app.rs:1403, 1469, 1561, 1580
  → src/tui/keybindings.rs:10363, 10475
  → src/tui/event_loop.rs:1311

1x error[E0061]: takes 7 arguments but 6 supplied
  → src/tui/keybindings.rs:22602 (PendingBrowseArchiveRename::new)

1x error[E0308]: match arms incompatible types
  → src/tui/keybindings.rs:7335
```

The missing argument is `target_inner_paths: Option<Vec<String>>` on `browse_active_staging_with_fingerprint` and similar functions. Pass `None` at call sites that don't have specific inner paths.

The match arm type mismatch at keybindings.rs:7335 is the same issue from previous rounds — the Details tab match returns `bool` from scroll functions but `()` from the Ctrl+R and analysis arms. Wrap the match in `{ ...; }` to discard the result, or make all arms return `bool`.

Full cargo check output is in `docs/cargo_check_output.txt`.
