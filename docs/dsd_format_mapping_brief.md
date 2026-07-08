# DSD Format Mapping: 3 Integration Test Failures

These are the last 3 test failures in the entire codebase.

## The Bug

DSD formats (`Dsf`, `Dff`) are not recognized as DSD by the TUI format state machinery. When the user selects DSF or DFF as the output format, the system treats it as a PCM format.

## Failures

All in `tests/tui_format_pipeline_settings.rs`:

```
pills_to_options_keeps_legacy_fields_consistent_for_dsd (line 82)
  → options.output_format is Flac instead of Dff

preset_v3_round_trips_new_format_fields (line 144)
  → format pill is Flac instead of Dsf after preset round-trip

all_format_families_have_expected_visible_rows_and_valid_pipeline_mapping (line 207)
  → state.is_dsd_selected() returns false for Dff
```

## Root Cause

All three failures suggest DSD formats are lost or not properly recognized somewhere in the pipeline. Note: `is_dsd_format()` at `app.rs:341` correctly matches `AudioFormat::Dsf | AudioFormat::Dff`, so the detection function itself is fine. The problem is upstream — DSF/DFF are likely not present in the format pill's option list, or `select_value(&AudioFormat::Dff)` silently fails because the value isn't in the pill options, leaving the pill on its default (Flac).

Trace the flow:
1. `FormatState::new()` — what formats are in `self.format.options`? Are Dsf/Dff included?
2. `PillState::select_value()` — does it silently no-op if the value isn't in the options list?
3. `apply_format_constraints()` — does it remove Dsf/Dff from enabled options under any conditions?
4. `TuiPreset::apply_to_pills()` — does the deserialized format string "dsf"/"dff" correctly map back to `AudioFormat::Dsf`/`AudioFormat::Dff`?
5. `try_pills_to_options()` — does it read the format pill correctly for DSD values?

## Files

- `tests/tui_format_pipeline_settings.rs` — the test file (included for reference)
- `src/tui/app.rs` — `FormatState`, `is_dsd_selected`, `apply_format_constraints`
- `src/tui/convert_actions.rs` — `try_pills_to_options`, `format_state_to_pipeline_settings`
- `src/tui/presets.rs` — `TuiPreset::from_pill_state`, `apply_to_pills`
- `src/convert/formats.rs` — `AudioFormat` enum

## Your Task

Fix the DSD format mapping so that DSF and DFF are correctly recognized, preserved through pills-to-options conversion, and round-tripped through presets. The passing PCM tests (Flac, Wav, etc.) must continue to pass.
