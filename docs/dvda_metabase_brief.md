# DVD-Audio Metabase — Implementation Brief

## Purpose

Implement DVD-Audio sidecar metadata support using foo_input_dvda's
metabase XML format. This enables MusicBrainz tagging, metadata
persistence, and tag transfer during conversion for DVD-Audio ISOs
and directories — the same workflow SACD already has via its sidecar
XML system.

---

## 1. The format

### File naming and location

Metabase files are XML files named `{STORE_ID}.xml` where `STORE_ID`
is a 32-character uppercase hex MD5 hash of the entire `AUDIO_TS.IFO`
file contents.

foo_input_dvda stores these in a central `dvda_metabase/` directory
inside the foobar2000 profile. tonepoet should support two locations:
1. **Sidecar**: same directory as the ISO, named `{STORE_ID}.xml`
   (mirrors the SACD sidecar pattern)
2. **Central catalog**: `~/.config/tonepoet/dvda_metabase/` (for
   compatibility with imported foo_input_dvda metabase files)

Search order: sidecar first, central catalog second.

### Store ID computation

From `dvda_metabase.cpp` line 61-79:
```cpp
auto tag_file = filesystem.open("AUDIO_TS.IFO");
auto tag_size = tag_file->get_size();
std::vector<std::byte> tag_data(tag_size);
tag_file->read(tag_data.data(), tag_size);
md5_string = hasher_md5::process_single(tag_data.data(), tag_size).asString();
```

The store ID is the MD5 hash of the complete `AUDIO_TS.IFO` file.
Uppercase hex, 32 characters. This is deterministic — the same disc
at different paths produces the same ID.

### XML structure

```xml
<?xml version="1.0" encoding="utf-8"?>
<!--DVD-Audio metabase file-->
<root>
  <store id="{STORE_ID}" type="DVD" version="1.1">
    <track id="1.2.3">
      <meta name="ARTIST" value="Artist Name"/>
      <meta name="TITLE" value="Track Title"/>
      <meta name="ALBUM" value="Album Name"/>
      <meta name="DATE" value="1985"/>
      <meta name="GENRE" value="Rock"/>
      <meta name="TRACKNUMBER" value="3"/>
      <meta name="TOTALTRACKS" value="9"/>
      <meta name="dvda_title" value="2"/>
      <meta name="dvda_titleset" value="1"/>
      <meta name="dvda_track" value="3"/>
    </track>
    <track id="1.2.4">
      ...
    </track>
  </store>
</root>
```

### Track ID format

`{titleset}.{title}.{track}` — e.g., `1.2.3` means ATS 1, title 2,
track 3. These map directly to tonepoet's:
- `titleset` → `TitleRef.title_set_nr`
- `title` → `AudioTitle.title_ordinal`
- `track` → `AudioChapter.track_nr`

The encoding from foo_input_dvda:
```cpp
auto subsong_to_string = [](auto subsong) {
    return string_printf("%d.%d.%d",
        (subsong >> 16) & 0xff,
        (subsong >> 8) & 0xff,
        subsong & 0xff);
};
```

### Meta keys

Standard tag keys: `ARTIST`, `TITLE`, `ALBUM`, `DATE`, `GENRE`,
`TRACKNUMBER`, `TOTALTRACKS`, `COMMENT`, `ALBUM ARTIST`

DVD-Audio-specific keys: `dvda_title`, `dvda_titleset`, `dvda_track`

Multi-value fields use `;` as separator.

### ReplayGain and album art

The format also supports `<replaygain>` tags and base64-encoded
`<albumart>` elements. These are lower priority for the initial
implementation.

---

## 2. What to implement

### 2a. Metabase parser + writer (`src/tui/dvda_metabase.rs`)

New module for reading and writing metabase XML files:

```rust
pub struct DvdaMetabase {
    pub store_id: String,
    pub tracks: Vec<DvdaMetabaseTrack>,
}

pub struct DvdaMetabaseTrack {
    pub id: String,                        // "1.2.3"
    pub meta: BTreeMap<String, String>,     // standard tag key-value pairs
}
```

Functions:
- `compute_store_id(volume: &dyn DvdaVolume) -> Option<String>` — MD5 of AUDIO_TS.IFO
- `find_metabase(iso_path: &Path, store_id: &str) -> Option<PathBuf>` — search sidecar then catalog
- `parse_metabase(path: &Path) -> Result<DvdaMetabase, Error>` — read XML
- `write_metabase(metabase: &DvdaMetabase, path: &Path) -> Result<(), Error>` — write XML
- `seed_from_disc(disc: &DvdaDisc, store_id: &str) -> DvdaMetabase` — create empty metabase with track IDs from parsed disc

### 2b. Integrate into the DVD-Audio mapper

When `map_dvda_disc()` or `map_dvda_source()` runs, check for an
existing metabase file. If found, populate `DiscContents.album_title`,
`album_artist`, `genre`, `year` from the metabase. Populate
`DiscTrack.title` and `DiscTrack.performer` from per-track meta.

Priority: metabase first, IFO/SAMG fallback (same pattern as SACD
sidecar priority).

### 2c. Integrate into the materializer

When `DvdaAudioMaterializer` builds `PreparedTrack`s, read metabase
for per-track metadata (TITLE, ARTIST, ALBUM, DATE, etc.) and populate
`TrackMetadata`. This enables tag transfer to converted output files.

### 2d. MusicBrainz tagging for DVD-Audio ISOs

When the user selects "Get tags from MusicBrainz" on a DVD-Audio ISO:

1. Parse the disc to get group/track structure
2. Select a group (default or user-chosen) to compute TOC
3. Compute CD-equivalent TOC sectors from PTS durations:
   `sectors = pts / 90000 * 75` (75 frames/sec CD rate)
4. Look up on MusicBrainz via TOC
5. Open the metadata editor with tracks from the selected group
6. On save, write metabase XML (sidecar next to ISO)

This mirrors the existing SACD flow in `command.rs` lines 2257-2323
and `keybindings.rs` `open_metadata_editor_for_sacd`.

### 2e. Import foo_input_dvda metabase files

Support loading metabase files from a user-specified directory
(e.g., the user copies their foobar2000 `dvda_metabase/` folder to
`~/.config/tonepoet/dvda_metabase/`). The store IDs are disc-specific
(MD5 of AUDIO_TS.IFO), so they'll match regardless of ISO file path.

---

## 3. Reference data

### 30 metabase XML files provided

At `dvda_metabase/` in the repo root. These are real foo_input_dvda
metabase files covering discs including:
- Pink Floyd — A Momentary Lapse of Reason
- Talking Heads — Little Creatures, Speaking in Tongues, Remain in Light, Naked
- Neil Young — Harvest
- Donald Fagen — The Nightfly
- R.E.M. — New Adventures in Hi-Fi, In Time
- Jackson Browne — Running on Empty
- David Bowie — David Live
- David Crosby — If I Could Only Remember My Name

### Store ID → disc mapping (from file contents)

The store ID is the MD5 of AUDIO_TS.IFO. To verify: open a known
DVD-Audio ISO, read AUDIO_TS.IFO, compute MD5, match against the
metabase filenames.

---

## 4. Code to read

```
foo_input_dvda reference:
  dvda_metabase.h       — class definition
  dvda_metabase.cpp     — full implementation (400 lines)
  audio_track.cpp       — metabase integration in track list builder

tonepoet existing SACD sidecar (pattern to follow):
  src/tui/sacd_sidecar.rs       — SidecarMetadata, parse/write, find_sidecar_for_iso
  src/tui/keybindings.rs        — open_metadata_editor_for_sacd
  src/tui/command.rs            — TagsFromMb SACD auto-detect block (lines 2257-2323)
  src/disc/sacd_mapper.rs       — sidecar priority in mapper

tonepoet DVD-Audio:
  src/disc/dvda_utils.rs        — map_dvda_source, probe_group_aob_format
  src/disc/dvda_mapper.rs       — map_dvda_disc
  crates/dvda-demuxer/src/tui/dvda/model.rs — DvdaDisc, AudioTitle, AudioChapter
```

---

## 5. What the reasoning model should produce

1. `src/tui/dvda_metabase.rs` — parser, writer, store ID computation,
   sidecar/catalog search
2. Modified `src/disc/dvda_mapper.rs` — metabase metadata priority
3. Modified `src/disc/dvda_utils.rs` — metabase loading in map_dvda_source
4. Modified `src/tui/command.rs` — TagsFromMb DVD-Audio handler
   (analogous to SACD block)
5. Modified `src/tui/keybindings.rs` — open_metadata_editor_for_dvda
6. Modified `src/tui/mod.rs` — declare dvda_metabase module
7. Tests for metabase parsing, store ID computation, and round-trip
   write/read
