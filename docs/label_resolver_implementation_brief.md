# Code task: LabelResolver implementation — dictionary lookups + business rules + artist canonicalization

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`

## Context

Read these files for background:
- `CLAUDE.md` — project overview, workspace structure
- `src/convert/pipeline/label_resolver.rs` — the existing `LabelResolver` trait, `StubLabelResolver`, `ResolvedLabel`, and `enrich_with_label_info` function
- `src/convert/pipeline/types.rs` — `AlbumMetadata`, `TrackMetadata` (both have `extra: BTreeMap<String, String>`)
- `src/convert/pipeline/stages.rs` — `render_folder_template`, `render_track_template`, `resolve_extra_tokens` (the template engine that consumes extra map values)
- `docs/naming_template_expansion_brief.md` — how the template system works

## Architecture overview

The system has three layers that compose via the `extra: BTreeMap<String, String>` on `AlbumMetadata`:

1. **Input layer** (metadata sources): materializers populate `extra` from tags, SACD TOC, CUE sheets, lofty enumeration. A future "discovery engine" (not part of this task) will also populate `extra` by scanning folder names, info.txt, nfo files, etc.

2. **Rules layer** (this task): the `LabelResolver` applies business rules — catalog prefix → country mapping, label canonicalization, premium label catalog suppression, country suppression for non-LP/non-Japan, Germany normalization. It enriches `extra` with `label`, `country`, `pressing` keys. Tag-sourced values are never overwritten.

3. **Output layer** (template engine): `render_folder_template` and `render_track_template` resolve `%LABEL%`, `%COUNTRY%`, `%PRESSING%`, `%ARTIST%` and any custom `%TOKEN%` from `extra` via `resolve_extra_tokens`.

**Critical design constraint**: any consumer that can populate an `AlbumMetadata.extra` map can use the full resolver + template chain. The discovery engine, the materializers, and manual tag editing all feed the same interface. The resolver must not assume where its input data came from.

## What already exists

**`LabelResolver` trait** (in `label_resolver.rs`):
```rust
pub trait LabelResolver: Send + Sync {
    fn resolve(&self, metadata: &AlbumMetadata, container: &Path) -> Option<ResolvedLabel>;
}
```

**`ResolvedLabel` struct**:
```rust
pub struct ResolvedLabel {
    pub label: Option<String>,
    pub country: Option<String>,
    pub pressing: Option<String>,
}
```

**`StubLabelResolver`**: returns `None` for all inputs.

**`enrich_with_label_info`**: called in the orchestrator (`run_pipeline_item_with_tool_paths`) after materialization, before `plan_outputs`. Uses `entry().or_insert()` so tag-sourced values in `extra` are never overwritten.

**Existing `labels.rs`** (in `src/convert/labels.rs`): has `detect_pressing_info(folder_name, year)` which parses folder names for pressing info. Has a `LabelInfo` struct. Has a 200+ entry `LABEL_MAPPINGS` table. This is legacy code used by `renaming.rs`. Do NOT modify it — the new resolver is a separate, parallel system.

**hexload-tui labels.rs** (vendored at `docs/hexload_labels_reference.rs`): READ THIS FILE. It has a much richer 370+ entry `LABEL_MAPPINGS` table that the new resolver should incorporate. It includes:
- Mastering engineers: RL, Sterling, KG (Kevin Gray), BG (Bernie Grundman), Wally Traugott, Doug Sax, Stan Ricker, Chris Bellman, Ryan Smith, Steve Hoffman, Piros, Gilbert Kong, Bob Weston
- Pressing plants: RTI, QRP, Pallas, Optimal, Monarch, TML, TML-M, Presswell, PRC, Allied, Capitol Winchester, Gloversville, Europadisk, MPO, Orlake, Damont, Terre Haute
- Country-specific labels that resolve the Harvest ambiguity: "UK Harvest" → UK, "Japan Harvest" → Japan (separate entries)
- UK labels: Vertigo, Island, Track, Decca, Columbia, Parlophone, Apple, Atlantic, Factory, 4AD, Creation, Rough Trade, Abbey Road, Townhouse, Tube Cut
- Japan labels: Warner Pioneer, Epic Sony, CBS-Sony, King Records, Odeon, Charisma, Toshiba Pro-Use, Mastersound, Polydor, Vertigo, Denon, A&M, Elektra, London, Philips
- German labels: Odeon, Teldec, Hörzu
- Audiophile labels list (26 entries) and reissue keyword list
- 7-inch/12-inch format parsing
- Audiophile prefix fallbacks for longest-match resolution

The new `DictionaryLabelResolver` should use this data as its primary label mapping source.

## What this task delivers

### 1. Replace StubLabelResolver with DictionaryLabelResolver

Create a new `DictionaryLabelResolver` struct that implements `LabelResolver`. It holds the dictionary data and applies the business rules.

**Dictionary data** (embed as static data or load from config — implementor's choice):

**Catalog prefix → country mapping** (80 entries, derived from the user's library of ~5,000 Japan releases):
```
# Original 22 entries
AMCE → Japan, AMCY → Japan, BVCP → Japan, CDSOL → Japan, KICJ → Japan,
MVCR → Japan, POCM → Japan, SICP → Japan, SRCS → Japan, TECW → Japan,
TOCJ → Japan, TYCJ → Japan, UCCI → Japan, UCCJ → Japan, UCCQ → Japan,
UICY → Japan, VICJ → Japan, VICP → Japan, VJCP → Japan, VRCL → Japan,
WPCR → Japan, SME → Japan,

# New entries discovered from ~/library/ (10+ occurrences in Japan context)
TOCP → Japan,   # Toshiba-EMI (329 occurrences)
MHCP → Japan,   # Sony Music Japan (160)
PHCR → Japan,   # Philips Japan (103)
VDJ → Japan,    # Victor Japan (77)
BVCM → Japan,   # BMG Victor (71)
UCCU → Japan,   # Universal Japan (63)
VACK → Japan,   # Victor (59)
PCD → Japan,    # Polydor Japan (56)
AIRAC → Japan,  # Air Records (49)
SICJ → Japan,   # Sony Japan (45)
UCCO → Japan,   # Universal Japan (37)
VDP → Japan,    # Victor Japan (24)
TODP → Japan,   # Toshiba-EMI (24)
WPCP → Japan,   # Warner-Pioneer (23)
UCCV → Japan,   # Universal Japan (20)
BVCJ → Japan,   # BMG Victor (20)
VJD → Japan,    # Victor Japan (19)
ESCA → Japan,   # Epic/Sony (19)
CSCS → Japan,   # CBS/Sony (19)
EICP → Japan,   # Epic International (18)
PPD → Japan,    # Polydor Japan (16)
MVCZ → Japan,   # (16)
BVCA → Japan,   # BMG Victor (16)
TECP → Japan,   # Toshiba-EMI (15)
WMC → Japan,    # Warner Music Japan (12)
POCD → Japan,   # Polydor Japan (12)
POCJ → Japan,   # Polydor Japan (10)
MVCG → Japan,   # (10)
VHCD → Japan,   # (8)
UICS → Japan,   # Universal Japan (8)
TECI → Japan,   # Toshiba-EMI (8)
PCCY → Japan,   # (8)
GQBS → Japan,   # (8)
COCB → Japan,   # Columbia Japan (8)
UCCE → Japan,   # Universal Japan (7)
MVCJ → Japan,   # (7)
WQCR → Japan,   # (6)
VSCD → Japan,   # (6)
UCCM → Japan,   # Universal Japan (6)
POCP → Japan,   # Polydor Japan (6)
MYCJ → Japan,   # (6)
AVCD → Japan,   # Avex (6)
KICP → Japan,   # King Records (5)
VICL → Japan,   # Victor (4)
UPCY → Japan,   # Universal Japan (4)
UCGO → Japan,   # Universal Japan (4)
TKCV → Japan,   # Tokuma Japan (4)
SHOUT → Japan,  # Shout! (Japan reissue label) (4)
PPDM → Japan,   # Polydor Japan (4)
PHDR → Japan,   # Philips Japan (4)
MVCI → Japan,   # (4)
MVCF → Japan,   # (4)
DIW → Japan,    # DIW Records (4)
COCY → Japan,   # Columbia Japan (4)
BSCP → Japan,   # (4)
BRJ → Japan,    # (4)
ALT → Japan,    # Altitude Records Japan (4)
```

**Label → country mapping** (13 entries):
```
Analogue Productions → US, Audio Fidelity → US, CBS-Sony → Japan,
CBS Sony → Japan, Esoteric → Japan, DCC → US, MFSL → US,
Nautilus → US, SHM → Japan, Toshiba → Japan, Toshiba-EMI → Japan,
Toshiba EMI → Japan, King Records → Japan
```

**Canonical label normalization** (12 canonical labels with variants):
```
Blue Note = [Blue Note, BlueNote, Bluenote, UMe]
Blue Note Classic = [Bluenote Classic, Blue Note Classic]
Blue Note Tone Poet = [Tonepoet, TonePoet, Tone Poet]
CBS-Sony = [CBS/Sony, CBS Sony, CBSSony, CBS_Sony, CBS.Sony, CBS - Sony, ...]
DCC = [DCC, DCC Compact Classics, Digital Compact Classics]
MFSL = [MFSL, Mofi, Mobile Fidelity, Mobile Fidelity Sound Lab, UDCD]
MFSL UltraDisc UHR = [MFSL UltraDisc UHR]
Nautilus SuperDisc = [Nautilus, Nautilus Recordings, Nautilus Super Disc, ...]
Pure Pleasure = [Pure Pleasure, Pure Pleasure Records]
Warner = [Warner Bros. Records, Warner Brothers Records, Warner Records, ...]
```

**Canonical country normalization** (10 entries):
```
AUS = [Australia, AUS, Australian]
CA = [Canada, CA, CDN, Canadian]
DE = [German, DE, Germany]
EU = [Europe, EU, European]
FR = [French, FR, France]
IT = [Italy, IT, Italian]
Japan = [JPN, JP, Japanese, Jap, Jpn, Japan]
NL = [Dutch, NL, Netherlands]
UK = [UK, United Kingdom, England, Great Britain, U.K.]
West German = [W. German, W. Germany, West Germany]
```

### 2. Resolution logic

The `resolve` method implements a waterfall:

**Step 1: Extract catalog prefix.** If `metadata.extra` contains a key `"catalog"` or `"sacd_album_catalog_number"`, extract the alphabetic prefix (regex: `^[A-Za-z]+`). Look up the prefix in the catalog-prefix-to-country mapping.

**Step 2: Determine label.** Check `metadata.extra` for existing `"label"` key (from tags). If present, normalize it through the canonical label mapping (case-insensitive match against all variant lists). If no tag-sourced label, check the container filename against the label variant lists (substring match, longest match wins).

**Step 3: Determine country.** Priority order:
1. Tag-sourced country (from `extra["country"]` or `extra["releasecountry"]` — note: all extra keys are lowercase per the `item_key_to_extra_key` normalization in materializer_7z)
2. Catalog prefix → country (from Step 1)
3. Label → country (from label-to-country mapping)
4. Container filename substring match against country variants

Normalize the result through canonical country mapping.

**Step 4: Apply business rules.**

**Premium label catalog suppression**: If the resolved label (after normalization) is one of: MFSL, DCC, Analogue Productions, Audio Fidelity, XRCD, XRCD2, XRCD24, Analog Spark, Esoteric — set `pressing` to the label name (just the canonical name, e.g., `"MFSL"`, NOT a formatted string) but do NOT include catalog in the resolved output. These labels ARE the identity; catalog is redundant. The template engine handles formatting — the user composes parts via `%PRESSING%` in templates like `{%COUNTRY% %PRESSING% %MEDIA%}`.

**Country suppression**: Only include country in the resolved output if the source appears to be LP/vinyl OR the country is Japan. Indicators for LP: `extra["media"]` contains "LP", or folder name contains LP/vinyl/180g/200g keywords. If neither LP nor Japan, suppress country.

**Digital download handling**: If the container filename or `extra` contains WEB/HDTracks/Qobuz/eOnkyo markers, set label to "DD" (or the service name). Suppress country and catalog.

**Germany normalization**: If country resolves to German/Germany/West Germany, check year. Pre-1990 → "West German". Post-1990 → "DE". Use `metadata.date` for year, extract 4-digit number.

**Step 5: Build ResolvedLabel.** Return the resolved label, country, and pressing (if any). `enrich_with_label_info` will insert these into `extra` without overwriting existing tag-sourced values.

### 3. Artist canonicalization

Add an `ArtistCanonicalizer` that normalizes artist names against a canonical list.

**Interface:**
```rust
pub struct ArtistCanonicalizer {
    // lowercase artist name → canonical casing
    canonical: HashMap<String, String>,
}

impl ArtistCanonicalizer {
    pub fn new() -> Self; // loads/embeds the canonical list
    pub fn canonicalize(&self, artist: &str) -> String;
}
```

**Logic:**
- Build a `HashMap<String, String>` mapping `artist.to_lowercase()` → canonical form
- `canonicalize()` looks up the lowercased input; returns canonical form if found, original input unchanged if not
- Never rejects — always returns a value
- The canonical list has 2,273 entries (vendored at `docs/canonical_artists_reference.txt`)

**Integration point:** Call the canonicalizer on the artist value in BOTH `render_folder_template` and `render_track_template`, after extracting the artist from metadata but before `sanitize_component`. This ensures `%ARTIST%` in templates always produces the canonical casing.

**Implementation constraint:** The render function signatures must NOT change. Use a module-level `once_cell::sync::Lazy<ArtistCanonicalizer>` (or `std::sync::LazyLock` if on Rust 1.80+) so the canonicalizer is initialized once and accessible within the render functions without passing it as a parameter. The data is immutable after construction, so a static is safe and idiomatic. Check `Cargo.toml` for whether `once_cell` is already a dependency; if not, `std::sync::LazyLock` is available in the nix-provided Rust toolchain.

### 4. Wire DictionaryLabelResolver into the orchestrator

In `run_pipeline_item_with_tool_paths` in `stages.rs`, replace `StubLabelResolver` with `DictionaryLabelResolver::new()`:

```rust
if let Some(ref mut src) = source {
    super::label_resolver::enrich_with_label_info(
        &mut src.album_metadata,
        &req.container,
        &super::label_resolver::DictionaryLabelResolver::new(),
    );
}
```

### 5. Data embedding

The dictionary data can be embedded as Rust constants/statics or loaded from TOML/JSON config files. Either approach is acceptable. If embedded, use `phf` or `lazy_static` with `HashMap` — whichever is simpler. The data is small (under 300 entries total plus 2,273 artist names).

If loading from files, use `~/.config/tonepoet/` as the config directory (consistent with existing config). Provide default embedded fallbacks so the resolver works without config files.

## Locked contracts (do not change)

- `LabelResolver` trait signature
- `ResolvedLabel` struct fields
- `enrich_with_label_info` function signature and behavior (entry().or_insert() semantics)
- `PipelineEvent` enum, `PipelineReporter` trait, `ProgressUpdate`, `ConversionStatus`
- Existing `labels.rs` and `renaming.rs` — do not modify
- Template engine functions (`render_folder_template`, `render_track_template`, `resolve_extra_tokens`) — do not change their signatures, only add artist canonicalization to the value resolution within them

## Files modified

| File | Changes |
|------|---------|
| `src/convert/pipeline/label_resolver.rs` | Add `DictionaryLabelResolver`, `ArtistCanonicalizer`, dictionary data, business rule logic |
| `src/convert/pipeline/stages.rs` | Wire `DictionaryLabelResolver` in orchestrator (replace `StubLabelResolver`); integrate `ArtistCanonicalizer` into render functions |
| `src/convert/pipeline/mod.rs` | Re-export new public types if needed |

## Tests required

**Dictionary lookups:**
- Catalog prefix "UCCQ" resolves to country "Japan"
- Catalog prefix "UNKNOWN" returns None
- Label "MoFi" normalizes to "MFSL"
- Label "Blue Note" stays "Blue Note" (already canonical)
- Country "JPN" normalizes to "Japan"
- Country "W. Germany" normalizes to "West German"

**Business rules:**
- MFSL label → catalog suppressed, pressing = "MFSL"
- Japan + any media → country preserved
- US + CD → country suppressed
- UK + LP → country preserved
- Germany + year 1985 → "West German"
- Germany + year 2005 → "DE"
- Container with "WEB" → label = "DD", country suppressed

**Artist canonicalization:**
- "miles davis" → "Miles Davis"
- "MILES DAVIS" → "Miles Davis"
- "Unknown Artist Not In List" → "Unknown Artist Not In List" (pass-through)
- "bill evans trio" → "Bill Evans Trio" (if in canonical list)

**Integration:**
- Full pipeline run with DictionaryLabelResolver produces correct folder names
- Tag-sourced label in extra is NOT overwritten by resolver
- StubLabelResolver tests still pass (backward compat)
- Existing 686 tests still pass

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`.

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Production-ready code changes to all files listed above. Must compile and pass `cargo test --lib` (currently 686 tests, must not regress).
