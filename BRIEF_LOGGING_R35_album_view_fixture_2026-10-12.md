# BRIEF — R34/R34C/R34D: four cleared, one moved

Gate: **7457 passed / 2 failed**, compile clean. Four of the five previously
failing tests pass. One of the two failures is the expected source-lock
freshness failure; requalification is owed since R33 edited `stages.rs`.

It did not compile as delivered: `src/tui/keybindings.rs:87181` used
`super::tag_interchange::` inside `mod phase4_tests`, where the neighbouring
references use `super::super::`. Fixed here. Sixth consecutive delivery that did
not compile, second instance of this same error.

## The remaining failure

`r33c_album_view_discnumber_follows_selected_carrier_authority` has moved, not
cleared. It now fails at `src/tui/keybindings.rs:103117`:

```
assert_eq!(state.active_surface().label, "Album view")
  left  "File tags"
  right "Album view"
```

Deterministic, 2 of 2 alone. The test's two halves differ only in CUE track
count. The first writes two tracks per image and clears the identical
`"Album view"` assertion at `:102945`. The second rewrites the same two CUEs
over the same two FLACs as one track each, selects individual-files-first
authority, and gets File tags.

**What we want:** a folder of one-track-per-file FLACs with sidecar CUEs
presents an Album view and resolves to the configured authority — or, if that
combination legitimately has no Album view, the test asserts what it does have.
