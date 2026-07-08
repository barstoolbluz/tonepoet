# Disc Subfolders Still Not Working

Third attempt. The `disc dirs` option is ticked in the UI but files are still not grouped into disc subfolders.

## Empirical Evidence

```
The Allman Brothers Band - At Fillmore East (1971) [FLAC] {MFSL}/
  01 - Hot 'Lanta.flac           (31M)
  01 - Statesboro Blues.flac     (25M)
  02 - Done Somebody Wrong.flac  (26M)
  02 - In Memory of Elizabeth Reed.flac (78M)
  03 - Stormy Monday.flac        (46M)
  03 - Whipping Post.flac       (131M)
  04 - You Don't Love Me.flac   (108M)
  artwork/
  conversion.log
```

Duplicate track numbers (two 01s, two 02s, two 03s) — this is clearly a 2-disc set. The source archive is a 7z containing two discs of the Allman Brothers' At Fillmore East.

## What Should Happen

With disc subfolders enabled, the output should be:
```
The Allman Brothers Band - At Fillmore East (1971) [FLAC] {MFSL}/
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

## Diagnosis Approach

Don't guess. Trace the actual data flow:

1. Check the conversion.log file in the output — it should contain per-track metadata including disc numbers. Does the pipeline know these tracks belong to different discs?

2. Trace `create_disc_subfolders` from `ConversionOptions` all the way to the point where output file paths are constructed. Is the field present in `PipelineRequest`? In `PipelineSettings`? In the publish plan? At the point where the final path is built?

3. Check if the source archive's internal structure contains disc-number information (folder structure like `Disc 1/`, `CD1/`, or DISCNUMBER tags). How does the pipeline detect multi-disc sets?

4. Check the publish stage — when `create_disc_subfolders` is true and disc numbers are available, does it actually create the subdirectories and route files into them?

5. If the field is wired but the disc detection doesn't fire, check the heuristics. The archive may not have explicit disc folders — it might just have duplicate track numbers, which requires a different detection strategy.

## Files

- `src/convert/pipeline/stages.rs` — publish stage, output path construction, disc detection
- `src/convert/pipeline/types.rs` — `PipelineRequest`, track metadata, disc number fields
- `src/tui/convert_actions.rs` — `create_disc_subfolders` wiring from UI to options
- `src/convert/formats.rs` — `ConversionOptions` struct
- `src/convert/processor.rs` — request construction
