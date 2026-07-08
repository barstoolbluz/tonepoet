# UX Batch Fix Round 3: 3 Unresolved Issues

Two of the biggest requests from the UX batch were not fixed, and a new issue was discovered. These are empirically observed — the user tested the app after the previous two patches.

## 1. Conditional template blocks `{...}` still dropping braces when content resolves

**Template:** `%ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%] {%TITLE_EXTRA%}`

**Expected output:** `The Allman Brothers Band - At Fillmore East (1971) [FLAC] {MFSL}`

**Actual output:** `The Allman Brothers Band - At Fillmore East (1971) [FLAC] MFSL`

The `%TITLE_EXTRA%` resolves to `MFSL` correctly, but the `{` and `}` are stripped from the output.

**The `{...}` syntax means:** "if all variables inside resolve, keep the braces as literal output characters wrapping the content; if any variable is empty, drop the entire block including the braces."

Correct behavior:
- `{%TITLE_EXTRA%}` with `MFSL` → `{MFSL}` (braces are literal, kept in output)
- `{%TITLE_EXTRA%}` with empty → nothing (entire block including braces dropped)

The current implementation strips the braces unconditionally regardless of whether the content resolved. Fix so that when the block resolves, the `{` and `}` are preserved as literal characters in the output.

## 2. Multi-disc files not grouped into disc subfolders

**Observed:** The user ticked `disc dirs` in the UI. The converted files for a 2-disc set are all in one directory:

```
The Allman Brothers Band - At Fillmore East (1971) [FLAC] MFSL/
  01 - Statesboro Blues.flac
  01 - Hot 'Lanta.flac
  02 - Done Somebody Wrong.flac
  02 - In Memory of Elizabeth Reed.flac
  03 - Stormy Monday.flac
  03 - Whipping Post.flac
  04 - You Don't Love Me.flac
  artwork/
  conversion.log
```

There are duplicate track numbers (two `01`, two `02`, two `03`) — clear evidence of a multi-disc set. Expected output with disc subfolders enabled:

```
The Allman Brothers Band - At Fillmore East (1971) [FLAC] MFSL/
  Disc 01/
    01 - Statesboro Blues.flac
    02 - Done Somebody Wrong.flac
    03 - Stormy Monday.flac
    04 - You Don't Love Me.flac
  Disc 02/
    01 - Hot 'Lanta.flac
    02 - In Memory of Elizabeth Reed.flac
    03 - Whipping Post.flac
  artwork/
  conversion.log
```

Investigate why the `create_disc_subfolders` option is not taking effect. Trace from the UI toggle through `ConversionOptions` → `PipelineRequest`/`PipelineSettings` → the publish/naming stage. The option may not be wired through, or the disc number detection may not be firing, or the publish stage may not be creating the subdirectories.

## 3. Relict lock files and staging directories left in the output directory

**Observed:** After conversion completes, the output directory contains:

```
~/temp/
  .10cc - Bloody Tourists (UK First-Press LP   24-96) [pbthal 2023] (1978) [FLAC] {}.lock
  .George Benson - White Rabbit (US CTI 6015 LP   24-96) (1972) [FLAC] {}.lock
  .Neil Young & The Santa Monica Flyers - Somewhere Under the Rainbow (1973) [FLAC] {}.lock
  .The Allman Brothers Band - At Fillmore East (1971) [FLAC] MFSL.lock
  .tonepoet-staging/
```

These `.lock` files and the `.tonepoet-staging/` directory should be cleaned up after conversion completes successfully. They are internal artifacts that should never be visible to the user in the final output.

Note the lock file names also reveal template rendering bugs — some show `{}` (empty conditional block rendered as literal braces) and some show double-space gaps where stripped content left whitespace behind.

Investigate:
- Why are `.lock` files not cleaned up after successful publish?
- Why is `.tonepoet-staging/` not cleaned up after all jobs complete?
- Is there a cleanup step that should run but doesn't, or is cleanup failing silently?

## Files

- `src/convert/pipeline/stages.rs` — template rendering (#1), disc subfolder logic (#2), publish cleanup (#3), lock file lifecycle (#3)
- `src/tui/convert_actions.rs` — `create_disc_subfolders` wiring (#2)
- `src/tui/app.rs` — output options state (#2)
- `src/convert/pipeline/types.rs` — `PipelineRequest` fields (#2)
- `src/convert/processor.rs` — staging cleanup (#3)
