# sacd-rs architecture audit

Assessment of the sacd-rs crate relative to how tonepoet actually consumes it.
Conducted 2026-06-09 at commit db92614.

## Verdict

The extraction engine is exactly as complex as it needs to be. The crate also
contains ~7K LOC that tonepoet never exercises: ~4.3K for a DST re-encoding
path that isn't exposed, and ~2.3K of unused dsd_file sub-modules and stubs.
Another ~2.3K of internal serialization modules (writers, id3, footer) are
`pub` but only consumed within the crate. Not overengineered — "ported
complete, consumed partial."

## What tonepoet actually calls

The entire SACD extraction flow touches five entry points in sacd-rs:

| Function / Type | File | Called from |
|---|---|---|
| `IsoReader::open()` | `iso_reader.rs` | `stages.rs` L1065 |
| `extract_track_with_area_frame_format()` | `extract.rs` L961 | `stages.rs` L1076 |
| `validate_dsd_stream()` | `dsd_file/ops.rs` L204 | `stages.rs` L8808 |
| `inspect_dsd_container()` | `dsd_file/inspect.rs` L219 | `plan_bridge.rs` |
| `FrameFormat::from_nibble()` | `frame.rs` | `tui/sacd.rs` L1966 |

Plus supporting types: `ExtractOptions`, `ExtractReport`, `ExtractStats`,
`OutputFormat`, `ExtractIntegrityOptions`, `DsdContainerInfo`,
`DsdContainerFormat`, `DsdCompression`, `DsdValidationMode`,
`DsdValidationOptions`.

## Actual extraction flow

```
User selects SACD ISO in browse screen
  |
  v
is_sacd_iso() detects it                     [src/tui/sacd.rs]
  |
  v
SacdIsoMaterializer parses TOC               [materializer_sacd.rs]
  -> parse_sacd_iso() reads Master TOC        [src/tui/sacd.rs — NOT sacd-rs]
  -> Builds PreparedSource with TrackSourceRef::SacdTrack
  |
  v
realize_sacd_track_blocking()                 [stages.rs L998]
  -> IsoReader::open(iso)                     [sacd-rs]
  -> area_info.track_extract_options()        [src/tui/sacd.rs — NOT sacd-rs]
  -> extract_track_with_area_frame_format()   [sacd-rs]
  -> Writes DSF or DFF to staging dir
  -> validate_sacd_realization()              [stages.rs]
  |
  v
validate_dsd_stream()                         [sacd-rs]
  |
  v
inspect_dsd_container()                       [sacd-rs]
  |
  v
Downstream stages transcode to final format
```

## Essential modules (~11K LOC)

These are exercised by the extraction path tonepoet actually uses:

| Module | LOC | Role |
|---|---|---|
| `extract.rs` | 3233 | Core extraction engine, integrity reporting |
| `frame.rs` | 2075 | Sector parser, frame assembly, time filter, dynamic tail trim |
| `dsd_file/reader.rs` | 2549 | DSD stream reading (internal to validate) |
| `dsd_file/inspect.rs` | 1611 | Container format detection |
| `dst/decoder.rs` | 1248 | DST-to-DSD frame decoding (called internally by extract) |
| `dsd_file/ops.rs` | 831 | DSD stream validation |
| `iso_reader.rs` | 110 | ISO sector-level I/O |

## Internal modules — used by extract, not by tonepoet directly

These are `pub` but only consumed within sacd-rs. They're implementation
details of the extraction engine, not dead code:

| Module | LOC | Role |
|---|---|---|
| `dsf_writer.rs` | 604 | DSF container serialization. Used by `extract.rs` for DSF output. |
| `dff_writer.rs` | 574 | Plain DFF container serialization. Used by `extract.rs` for DFF output. |
| `id3.rs` | 522 | ID3v2.4 tag rendering. Used by `extract.rs` and `dff_footer.rs` for metadata embedding. |
| `dff_footer.rs` | 561 | DIIN/COMT/ID3 footer assembly. Used by `extract.rs` for DFF metadata. |

These could be made `pub(crate)` to reduce the public API surface, but
they're not dead weight — they run on every extraction.

## Dead code (~4.5K LOC)

### Never exercised by tonepoet

| Module | LOC | Status |
|---|---|---|
| `dst/encoder.rs` | 2190 | DST compression engine. Used internally by `dff_dst_writer.rs`, but tonepoet never configures `DstExtractionOptions` — extraction always produces plain DSD. |
| `dff_dst_writer.rs` | 2119 | DST-compressed DFF output. Wired into `extract.rs` but only activated when `DstExtractionOptions` are provided, which tonepoet never does. |

These 4309 LOC compile and are tested but never execute in production.
They exist for a DST re-encoding capability that isn't exposed.

### Never used outside the crate or its tests

| Module | LOC | Status |
|---|---|---|
| `dsd_file/source.rs` | 714 | DSD file source abstraction. Only used in its own tests. |
| `dsd_file/asset.rs` | 532 | DSD asset model. Only used in its own tests. |
| `dsd_file/corpus.rs` | 306 | DSD corpus builder. Only used in its own tests. |
| `dsd_file/policy.rs` | 166 | DSD policy types. Only used in its own tests. |
| `dsd_file/metadata.rs` | 39 | DSD metadata trait. Unused. |
| `output_transaction.rs` | 549 | Transactional output wrapper. Exported, never imported. |
| 6 stub files | 42 | Empty trait stubs (asset_model, container, corpus, source_model, stream_ops, stream_reader). |

### Possible actions

- **Make internal modules `pub(crate)`** — reduces API surface, no behavior change.
- **Delete the DST encoder + DST writer** if there's no planned use case for
  DST-compressed DFF output (4309 LOC).
- **Delete unused `dsd_file/` sub-modules** and stubs (~2348 LOC).
- **Or leave it.** It compiles, it's tested, it adds no runtime cost. The dead
  code doesn't slow builds meaningfully at current scale.

## Metadata ownership split

This is the most notable architectural quirk:

- **`src/tui/sacd.rs`** (4620 LOC) owns all ScarletBook metadata parsing:
  Master TOC, area TOC, TRL1 (track sector ranges), TRL2 (track
  timecodes/durations), per-track text metadata, ISRC, frame format
  classification, and `track_extract_options()` which builds the
  `ExtractOptions` struct that sacd-rs consumes.

- **`crates/sacd-rs/`** owns audio extraction only: reading raw DSD frames from
  sector ranges, DST decoding, time filtering, and serializing to DSF/DFF
  containers. It cannot open an ISO and discover what's on it.

This means sacd-rs is not self-contained — it cannot be used as a standalone
library without the TUI module providing TOC parsing. The crate's own lib.rs
documents this explicitly:

> "This crate handles audio extraction [...] It does not parse ScarletBook
> metadata — that lives in tonepoet's `tui::sacd` module."

### Why it's this way

The TOC parser predates sacd-rs. It was written for the browse screen (disc
info display, track listing, area selection) and lived in the TUI because
that's where the UI consumed it. When sacd-rs was built for extraction, it
was designed to accept pre-parsed metadata rather than duplicate the parsing.

### If it ever matters

If sacd-rs needs to be a standalone library (e.g., for CLI-only extraction
without the TUI, or as a published crate), the TOC parsing from
`src/tui/sacd.rs` would need to move into the crate. The extraction types
(`ExtractOptions`, `FrameFormat`, `TimeFilter`) are already in sacd-rs —
it's the upstream TOC-to-options bridge that's missing.

This is not urgent. The current split works fine for tonepoet's architecture
where the TUI is the primary interface and the CLI convert path goes through
the same materializer.
