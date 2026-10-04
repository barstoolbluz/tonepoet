# Brief R26 — R24 corrective R3: three deterministic failures

Date: 2026-10-04
Base: the supplied `tonepoet-src.tar.gz` is `main` with the R24 corrective R3
snapshot applied unchanged. It compiles with zero warnings; no edits were
needed.

Gate 7327 passed / 5 failed across 63 targets, up from 7299 / 1.

## Expected

`installed_reference_qualification_binds_the_current_locked_sources`. The
snapshot changes four Reference-locked files: `plan_bridge.rs`, `stages.rs`,
`track_executor.rs`, `semantic_plan.rs`. Requalification runs on this host.

## Passes in isolation

`concurrency::tests::same_process_journal_coholder_shares_deliberate_export_authority`

## The three

Each fails in isolation as well as in the suite.

### `tests/tui_format_pipeline_settings.rs:96`

`auto_dither_selects_defaults_and_preserves_manual_choice`

```
left:  None
right: Shibata
```

This test predates R24 and was passing before this snapshot.

### `src/tui/convert_actions.rs`

`lifecycle_forwarder_tests::float_pcm_target_preserves_explicit_dither_choice_but_projects_it_inactive`

```
assertion failed: !format.dither.options[format.dither.selected].enabled
```

### `src/tui/keybindings.rs:81951`

`phase4_tests::r24_notice_overlay_cancel_hitbox_closes_the_notice`

```
assertion failed: matches!(app.active_overlay, ActiveOverlay::None)
```

The second and third are new in this snapshot. `VERIFICATION_R24_CORRECTIVE_R3_2026-10-04.txt`
records 34/34 focused assertions passing; those assertions are target-free and
were not compiled.

## The outcome we want

The gate is green, apart from the Reference requalification which runs here.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so.
