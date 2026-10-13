# BRIEF — R35: the test asserts an empty row is absent; every File tags surface has it

Gate: **7457 passed / 2 failed**, compile clean and no local fixes needed. One
failure is the expected source-lock freshness failure; requalification is owed.

## The remaining failure

`r33c_album_view_discnumber_follows_selected_carrier_authority`, deterministic,
2 of 2 alone, now at `src/tui/keybindings.rs:103149`:

```
assertion failed: entries.iter().all(|e| !e.display_key.eq_ignore_ascii_case("DISCNUMBER"))
```

`ensure_standard_fields_present` (`src/tui/probe.rs:8556`) pushes an empty entry
for every member of `CORE_EDITOR_FIELDS` that a file lacks, and `DISCNUMBER` is
one of its thirteen. `read_all_tags_merged` reaches it through
`sort_entries_standard_first_with_file_count`, so a DISCNUMBER row is present on
every File tags surface and the assertion cannot hold for any file.
`sort_entries_standard_first_existing_only` is the variant that does not inject.

The row is not coming from disk. The fixture FLACs are bare sine tones with no
tags, and the byte-identity assertions at `:103034` and `:103086` pass.

The step after the assertion is affected by the same thing: it adds DISCNUMBER
through the Add Key handler, but the row already exists.

**What we want:** the test works with the row the editor always provides — set
its value rather than add the key — and asserts the saved result lands on the
files with both CUEs unchanged.

## Three rounds, three assertions

| round | assertion |
| --- | --- |
| R33C | `:102795` all sides resolve to `IndividualFiles` |
| R34 | `:103117` active surface is `"Album view"` |
| R35 | `:103149` no pre-existing DISCNUMBER row |

Each corrective fixed the assertion it was given and the next line failed. The
test has not been run upstream in any round.
