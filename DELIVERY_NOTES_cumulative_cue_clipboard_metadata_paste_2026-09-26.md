# Cumulative delivery: CUE menu + unified clipboard + metadata FieldSet paste

Date: 2026-09-26

This is an iterative cumulative delivery. It was rebuilt by extracting the previously delivered corrected bundle:

- `tonepoet_cue_menu_clipboard_fixes_delivery_2026-09-26.tar.gz`
- SHA-256: `cf2792f4cdac5c3c130c8a887c254a2ab0414c00923df97e9cc42420e8dc7a47`

and then applying the metadata FieldSet follow-up patch directly to that extracted tree:

- `tonepoet_metadata_fieldset_paste_fixes_2026-09-26.patch`
- SHA-256: `b16b0fbad898866fe4cd4c583aecffdaac1d1da38b286454611ccc5ef2d2b550`

The prior CUE-menu and host-authoritative filesystem-clipboard corrections remain present. The follow-up adds only the metadata FieldSet paste corrections and their tests/documentation.

## Follow-up paths changed relative to the previous corrected bundle

- `DELIVERY_NOTES_metadata_fieldset_paste_fixes_2026-09-26.md` (new)
- `src/tui/app.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/message.rs`
- `src/tui/tag_interchange.rs`

This cumulative note is also new in the final archive.

## Validation performed here

- The metadata patch applies cleanly to a fresh extraction of the previous corrected bundle.
- The cumulative tree retains the earlier folder-classification request ownership/CUE fingerprint implementation.
- The cumulative tree retains host-authoritative filesystem clipboard matching and paste routing.
- The cumulative tree contains the new rowless metadata paste routing and unified-CUE semantic-dimension FieldSet regression coverage.
- The final tarball is re-extracted and compared byte-for-byte with the packaged working tree.

This runner has no `cargo`, `rustfmt`, or `nix`, so compilation, formatting, and workspace tests were not executable here. Run the repository's normal pinned gate before handoff.
