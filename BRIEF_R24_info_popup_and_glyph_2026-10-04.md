# Brief R24 — the information pop-up and its glyph

Date: 2026-10-04
Base: the supplied `tonepoet-src.tar.gz` is `main` with R23 applied. Gate 7299
passed / 1 failed, the failure a wandering contention flake that passes in
isolation and differs between runs; `tonepoet-true-peak` 160 / 0; Reference
qualification current.

Three things about the `ⓘ` pop-up the de-emphasis row opens.

## 1. It is a fixed size and looks cramped

`draw_notice` renders through the shared scrollable popup with
`preferred_width: 66` and `min_height: 9`.

Ratio-based sizing already exists elsewhere in the TUI —
`centered_rect(percent_x, percent_y, area)` in `src/tui/disc_browser.rs` and
`src/tui/conversion_actions_ui.rs`.

The pop-up sizes itself to the terminal the way those prompts do, and its text
is not cramped.

## 2. The `Esc` pill is not clickable

The footer renders `footer_pill("Esc close", theme.purple, theme)`.
`draw_scrollable_message_popup` receives no button map, so nothing registers a
hitbox for it. `draw_file_input`, `draw_text_edit` and `draw_command_input`
each receive one.

The `Esc` pill closes the pop-up when clicked, as it appears to offer.

## 3. The `ⓘ` does not read as an affordance

It renders as plain text in the row suffix — `pill_row` takes `suffix: &str` —
so it carries no colour of its own and sits on the default ground.

The glyph reads as something to click: a solid background and a contrasting
foreground, both drawn from the active theme, consistent across themes.

## The outcome we want

The information pop-up is sized like the TUI's other prompts, its `Esc` pill
works when clicked, and the `ⓘ` that opens it looks like a control.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so. Some files are Reference-source-locked
(`reference_source_lock.rs` lists them); if your change touches any, say so and
we requalify here.
