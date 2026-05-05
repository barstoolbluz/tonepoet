//! MusicBrainz disc-TOC lookup. Used by `:cue-mb` (overwrite) and
//! `:cue-fill` (enrich) to populate per-track titles, performers, ISRCs,
//! and album-level catalog/barcode data when the local rip is sparsely
//! tagged.
//!
//! Endpoint: `GET https://musicbrainz.org/ws/2/discid/-?toc=...&fmt=json`
//! TOC values are 1-based: `first+last+leadout+offset_1+...+offset_N`,
//! all in absolute sectors (with the standard 150-frame leadin).
//!
//! MusicBrainz rate-limit policy: 1 request/sec per IP, with a User-Agent.
//! We respect by setting a UA and relying on the SQLite cache to coalesce
//! repeated lookups. Single-shot calls never need an explicit sleep.

/// Parsed MusicBrainz release matching a disc TOC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbRelease {
    pub release_id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<String>,
    pub catalog: Option<String>,
    pub barcode: Option<String>,
    pub tracks: Vec<MbTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbTrack {
    pub position: u32,
    pub title: String,
    pub artist: String,
    pub isrc: Option<String>,
}

/// Build the MusicBrainz `toc=` query value from absolute sector offsets.
///
/// `sectors` must contain at least two entries: track 1 start, …, track N
/// start, leadout. All values are absolute sector positions (LBA + 150).
/// This is exactly what `accuraterip::find_toc_offsets` returns.
pub fn build_mb_toc(sectors: &[u32]) -> Option<String> {
    if sectors.len() < 2 {
        return None;
    }
    let n_tracks = sectors.len() - 1;
    let leadout = sectors[n_tracks];
    let track_offsets = &sectors[..n_tracks];

    let mut parts = Vec::with_capacity(3 + n_tracks);
    parts.push("1".to_string());
    parts.push(n_tracks.to_string());
    parts.push(leadout.to_string());
    for o in track_offsets {
        parts.push(o.to_string());
    }
    Some(parts.join("+"))
}

const MB_BASE: &str = "https://musicbrainz.org/ws/2/discid/-";
const USER_AGENT: &str = concat!(
    "tonepoet/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/barstoolbluz/tonepoet)"
);

/// Result of a TOC lookup: the parsed release (if matched) and the raw
/// JSON body the caller should write to the cache. `cache_response` is
/// `None` when the lookup was satisfied from a passed-in cached body so
/// the caller can skip a redundant store.
#[derive(Debug)]
pub struct MbLookupOutcome {
    pub release: Option<MbRelease>,
    pub cache_response: Option<String>,
}

/// Look up the best-matching MusicBrainz release for a disc TOC.
///
/// Database-free for use inside `tokio::spawn`: caller owns cache
/// retrieval (pass the cached JSON body via `cached_response`) and cache
/// storage (write `outcome.cache_response` back if `Some`). On cache hit
/// the function does no HTTP.
///
/// `Ok(MbLookupOutcome { release: None, .. })` means "no release matched
/// this TOC"; `Err(_)` is a transport/parse failure the caller should
/// surface.
pub async fn lookup_release_by_toc(
    sectors: &[u32],
    cached_response: Option<String>,
) -> Result<MbLookupOutcome, String> {
    let toc = build_mb_toc(sectors)
        .ok_or_else(|| "TOC must have at least 2 sector entries".to_string())?;
    let n_tracks = sectors.len() - 1;

    if let Some(json) = cached_response {
        return Ok(MbLookupOutcome {
            release: parse_mb_response(&json, n_tracks)?,
            cache_response: None,
        });
    }

    let url = format!(
        "{}?toc={}&inc=artist-credits+isrcs+labels+recordings&fmt=json",
        MB_BASE, toc,
    );

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(&url).send().await
        .map_err(|e| format!("MusicBrainz query failed: {}", e))?;

    let status = resp.status();
    let body = resp.text().await
        .map_err(|e| format!("MusicBrainz response error: {}", e))?;

    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(MbLookupOutcome {
            release: None,
            cache_response: Some(body),
        });
    }
    if !status.is_success() {
        return Err(format!("MusicBrainz returned HTTP {}", status));
    }

    let release = parse_mb_response(&body, n_tracks)?;
    Ok(MbLookupOutcome {
        release,
        cache_response: Some(body),
    })
}

/// Parse a MusicBrainz JSON response and select the best release.
/// Returns `Ok(None)` when no releases match; `Err(_)` on JSON parse
/// failure or unexpected schema.
///
/// `n_tracks` is the track count implied by the queried TOC; it disambiguates
/// the correct medium for multi-disc releases.
pub fn parse_mb_response(body: &str, n_tracks: usize) -> Result<Option<MbRelease>, String> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(body)
        .map_err(|e| format!("MusicBrainz JSON parse error: {}", e))?;

    // 404 body shape: {"error": "..."} → treat as miss.
    if v.get("error").is_some() {
        return Ok(None);
    }

    let releases = match v.get("releases").and_then(|r| r.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(None),
    };

    // MB does not always include `score`; fall back to the first release.
    let pick = releases
        .iter()
        .max_by_key(|r| r.get("score").and_then(|s| s.as_i64()).unwrap_or(0))
        .unwrap_or(&releases[0]);

    Ok(Some(release_from_json(pick, n_tracks)))
}

fn release_from_json(rel: &serde_json::Value, n_tracks: usize) -> MbRelease {
    let release_id = rel.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = rel.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let artist = artist_credit_string(rel.get("artist-credit"));
    let year = rel
        .get("date")
        .and_then(|v| v.as_str())
        .and_then(|s| s.get(..4).map(|y| y.to_string()));
    let barcode = rel
        .get("barcode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let catalog = rel
        .get("label-info")
        .and_then(|v| v.as_array())
        .and_then(|labels| {
            labels.iter().find_map(|li| {
                li.get("catalog-number")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
        });

    // For multi-disc releases, MB returns a media[] entry per disc. Pick the
    // medium whose track count matches the queried TOC; fall back to the
    // first medium if no exact match (single-disc releases or unusual data).
    let media = rel
        .get("media")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let pick_medium = media
        .iter()
        .find(|m| {
            let count = m.get("track-count").and_then(|c| c.as_u64())
                .unwrap_or_else(|| {
                    m.get("tracks").and_then(|t| t.as_array())
                        .map(|a| a.len() as u64).unwrap_or(0)
                });
            count == n_tracks as u64
        })
        .or_else(|| media.first());
    let tracks = pick_medium
        .and_then(|m| m.get("tracks"))
        .and_then(|t| t.as_array())
        .map(|tracks| tracks.iter().filter_map(track_from_json).collect())
        .unwrap_or_default();

    MbRelease { release_id, title, artist, year, catalog, barcode, tracks }
}

fn track_from_json(t: &serde_json::Value) -> Option<MbTrack> {
    let position = t.get("position").and_then(|v| v.as_u64())? as u32;
    let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let track_artist = artist_credit_string(t.get("artist-credit"));
    let artist = if track_artist.is_empty() {
        artist_credit_string(t.get("recording").and_then(|r| r.get("artist-credit")))
    } else {
        track_artist
    };
    let isrc = t
        .get("recording")
        .and_then(|r| r.get("isrcs"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(MbTrack { position, title, artist, isrc })
}

/// Populate a metadata editor state with values from a MusicBrainz release.
/// Track-level fields (TITLE, ARTIST, TRACKNUMBER, ISRC) come from the
/// matching `MbTrack.position`; album-level fields (ALBUM, DATE,
/// CATALOGNUMBER) apply to every track. Existing per-file tag values are
/// only overwritten when the MB value is non-empty — empty MB fields
/// preserve whatever the file currently has.
///
/// Mirrors `super::gnudb::populate_editor_from_gnudb`.
pub fn populate_editor_from_mb(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) {
    use lofty::tag::ItemKey;

    let n = state.paths.len();

    fn find_or_create(
        entries: &mut Vec<crate::tui::probe::TagEntry>,
        key: &str,
        item_key: ItemKey,
        n: usize,
    ) -> usize {
        if let Some(i) = entries.iter().position(|e| e.display_key.eq_ignore_ascii_case(key)) {
            return i;
        }
        entries.push(crate::tui::probe::TagEntry {
            display_key: key.to_string(),
            item_key,
            value: String::new(),
            original: String::new(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec![String::new(); n],
            per_file_originals: vec![String::new(); n],
        });
        entries.len() - 1
    }

    let title_idx = find_or_create(&mut state.entries, "TITLE", ItemKey::TrackTitle, n);
    let artist_idx = find_or_create(&mut state.entries, "ARTIST", ItemKey::TrackArtist, n);
    let album_idx = find_or_create(&mut state.entries, "ALBUM", ItemKey::AlbumTitle, n);
    let tn_idx = find_or_create(&mut state.entries, "TRACKNUMBER", ItemKey::TrackNumber, n);
    let date_idx = find_or_create(&mut state.entries, "DATE", ItemKey::Year, n);
    let isrc_idx = find_or_create(&mut state.entries, "ISRC", ItemKey::Isrc, n);
    let catalog_idx = find_or_create(
        &mut state.entries,
        "CATALOGNUMBER",
        ItemKey::CatalogNumber,
        n,
    );

    // Catalog: prefer label catalog number; fall back to barcode.
    let catalog_value = release.catalog.as_deref()
        .or(release.barcode.as_deref())
        .unwrap_or("")
        .to_string();

    for i in 0..n {
        // Match MB track by 1-based position; tolerate gaps.
        let mt = release.tracks.iter().find(|m| m.position as usize == i + 1);

        if let Some(mt) = mt {
            if !mt.title.is_empty() {
                state.entries[title_idx].per_file_values[i] = mt.title.clone();
            }
            if !mt.artist.is_empty() {
                state.entries[artist_idx].per_file_values[i] = mt.artist.clone();
            }
            if let Some(isrc) = mt.isrc.as_deref().filter(|s| !s.is_empty()) {
                state.entries[isrc_idx].per_file_values[i] = isrc.to_string();
            }
        }

        if !release.title.is_empty() {
            state.entries[album_idx].per_file_values[i] = release.title.clone();
        }
        // Track number is always 1-based by file position. (MB position
        // matches this in practice.)
        state.entries[tn_idx].per_file_values[i] = (i + 1).to_string();
        if let Some(year) = release.year.as_deref().filter(|s| !s.is_empty()) {
            state.entries[date_idx].per_file_values[i] = year.to_string();
        }
        if !catalog_value.is_empty() {
            state.entries[catalog_idx].per_file_values[i] = catalog_value.clone();
        }
    }

    // Recalculate the merged display value + mixed state per touched entry.
    for idx in [title_idx, artist_idx, album_idx, tn_idx, date_idx, isrc_idx, catalog_idx] {
        let entry = &mut state.entries[idx];
        let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
        entry.is_mixed = !all_same && n > 1;
        entry.value = if entry.is_mixed {
            "<multiple values>".to_string()
        } else {
            entry.per_file_values.first().cloned().unwrap_or_default()
        };
    }

    state.dirty = true;
}

/// Render a MusicBrainz `artist-credit` array into a single performer
/// string with `joinphrase` separators preserved.
fn artist_credit_string(value: Option<&serde_json::Value>) -> String {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut out = String::new();
    for entry in arr {
        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
            out.push_str(name);
        } else if let Some(name) = entry
            .get("artist")
            .and_then(|a| a.get("name"))
            .and_then(|v| v.as_str())
        {
            out.push_str(name);
        }
        if let Some(jp) = entry.get("joinphrase").and_then(|v| v.as_str()) {
            out.push_str(jp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_mb_toc_basic() {
        // 9 tracks, leadout at 293421 (matches our Allman SHM disc 1 TOC
        // before subtracting the 150-frame leadin).
        let sectors = vec![150, 19515, 36358, 51913, 72407, 112096, 134447, 193500, 280413, 293571];
        let toc = build_mb_toc(&sectors).unwrap();
        assert_eq!(
            toc,
            "1+9+293571+150+19515+36358+51913+72407+112096+134447+193500+280413"
        );
    }

    #[test]
    fn build_mb_toc_too_short_returns_none() {
        assert!(build_mb_toc(&[]).is_none());
        assert!(build_mb_toc(&[150]).is_none());
        assert!(build_mb_toc(&[150, 1000]).is_some());
    }

    #[test]
    fn parse_mb_response_picks_highest_score() {
        let body = r#"{
            "releases": [
                { "id": "low", "score": 50, "title": "Low", "media": [] },
                { "id": "high", "score": 100, "title": "High", "media": [] }
            ]
        }"#;
        let r = parse_mb_response(body, 0).unwrap().unwrap();
        assert_eq!(r.release_id, "high");
        assert_eq!(r.title, "High");
    }

    #[test]
    fn parse_mb_response_returns_none_on_empty() {
        assert!(parse_mb_response(r#"{"releases":[]}"#, 0).unwrap().is_none());
        assert!(parse_mb_response(r#"{"error":"not found"}"#, 0).unwrap().is_none());
    }

    #[test]
    fn parse_mb_response_picks_medium_matching_track_count() {
        // Multi-disc release: 2 media (10 tracks, 9 tracks). Querying with
        // n_tracks=9 must select the second medium.
        let body = r#"{
          "releases": [{
            "id": "rid", "title": "Album", "media": [
              {"track-count": 10, "tracks": [
                {"position": 1, "title": "wrong-1"},
                {"position": 2, "title": "wrong-2"}
              ]},
              {"track-count": 9, "tracks": [
                {"position": 1, "title": "right-1"},
                {"position": 2, "title": "right-2"}
              ]}
            ]
          }]
        }"#;
        let r = parse_mb_response(body, 9).unwrap().unwrap();
        assert_eq!(r.tracks[0].title, "right-1");
        assert_eq!(r.tracks[1].title, "right-2");
    }

    #[test]
    fn parse_mb_response_extracts_tracks_and_isrc() {
        let body = r#"{
          "releases": [{
            "id": "rid", "title": "Album",
            "artist-credit": [{"name": "Artist"}],
            "date": "1971-07-06",
            "barcode": "0044007735428",
            "label-info": [{"catalog-number": "UICY-94626"}],
            "media": [{
              "tracks": [
                {"position": 1, "title": "Track 1",
                 "recording": {"artist-credit": [{"name":"Artist"}],
                               "isrcs": ["USRC17607839"]}},
                {"position": 2, "title": "Track 2",
                 "recording": {"isrcs": []}}
              ]
            }]
          }]
        }"#;
        let r = parse_mb_response(body, 0).unwrap().unwrap();
        assert_eq!(r.title, "Album");
        assert_eq!(r.artist, "Artist");
        assert_eq!(r.year.as_deref(), Some("1971"));
        assert_eq!(r.barcode.as_deref(), Some("0044007735428"));
        assert_eq!(r.catalog.as_deref(), Some("UICY-94626"));
        assert_eq!(r.tracks.len(), 2);
        assert_eq!(r.tracks[0].title, "Track 1");
        assert_eq!(r.tracks[0].isrc.as_deref(), Some("USRC17607839"));
        assert_eq!(r.tracks[1].isrc, None);
    }

    #[test]
    fn mb_cache_round_trip() {
        let db = crate::db::Database::open_memory().expect("open memory db");
        let toc = "1+9+293571+150+19515+36358+51913";
        let body = r#"{"releases":[{"id":"x","title":"X","media":[]}]}"#;
        assert!(db.get_cached_mb_response(toc).is_none());
        db.store_mb_response(toc, body).unwrap();
        assert_eq!(db.get_cached_mb_response(toc).as_deref(), Some(body));
    }

    #[test]
    fn populate_editor_from_mb_fills_track_and_album_fields() {
        use crate::tui::app::{MetadataEditorPhase, MetadataEditorState};

        let mut state = MetadataEditorState {
            paths: vec![
                std::path::PathBuf::from("/tmp/01.flac"),
                std::path::PathBuf::from("/tmp/02.flac"),
            ],
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false,
            deleted: Vec::new(),
            file_labels: vec!["01".into(), "02".into()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
        };
        let release = MbRelease {
            release_id: "x".to_string(),
            title: "Album".to_string(),
            artist: "Artist".to_string(),
            year: Some("1971".to_string()),
            catalog: Some("UICY-94626".to_string()),
            barcode: Some("0044007735428".to_string()),
            tracks: vec![
                MbTrack {
                    position: 1, title: "Track 1".into(),
                    artist: "Artist".into(), isrc: Some("USRC17607839".into()),
                },
                MbTrack {
                    position: 2, title: "Track 2".into(),
                    artist: "Artist".into(), isrc: None,
                },
            ],
        };
        populate_editor_from_mb(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.entries.iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(key))
                .map(|e| e.per_file_values.clone())
                .unwrap_or_default()
        };
        assert_eq!(lookup("TITLE"), vec!["Track 1", "Track 2"]);
        assert_eq!(lookup("ARTIST"), vec!["Artist", "Artist"]);
        assert_eq!(lookup("ALBUM"), vec!["Album", "Album"]);
        assert_eq!(lookup("TRACKNUMBER"), vec!["1", "2"]);
        assert_eq!(lookup("DATE"), vec!["1971", "1971"]);
        assert_eq!(lookup("ISRC"), vec!["USRC17607839", ""]);
        // Catalog prefers label catalog over barcode.
        assert_eq!(lookup("CATALOGNUMBER"), vec!["UICY-94626", "UICY-94626"]);
        assert!(state.dirty);
    }

    #[test]
    fn artist_credit_joinphrase_preserved() {
        let v: serde_json::Value = serde_json::from_str(r#"[
            {"name": "Foo", "joinphrase": " & "},
            {"name": "Bar"}
        ]"#).unwrap();
        assert_eq!(artist_credit_string(Some(&v)), "Foo & Bar");
    }
}
