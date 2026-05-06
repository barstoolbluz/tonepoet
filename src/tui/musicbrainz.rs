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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MbRelease {
    pub release_id: String,
    pub release_group_id: Option<String>,
    pub artist_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub year: Option<String>,
    /// First-release-date of the release-group (year only). Distinct
    /// from `year`, which is *this* pressing's date.
    pub original_date: Option<String>,
    /// ISO country code, e.g. "US" / "JP".
    pub country: Option<String>,
    pub catalog: Option<String>,
    pub barcode: Option<String>,
    pub tracks: Vec<MbTrack>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MbTrack {
    pub position: u32,
    pub track_id: Option<String>,
    pub recording_id: Option<String>,
    pub artist_id: Option<String>,
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

/// Result of a TOC lookup: all matching releases sorted by descending
/// score, plus the raw JSON body the caller should write to the cache.
/// `cache_response` is `None` when the lookup was satisfied from a
/// passed-in cached body so the caller can skip a redundant store.
#[derive(Debug)]
pub struct MbLookupOutcome {
    /// All matching releases, highest-scoring first. Empty when MB
    /// returned no match (HTTP 404 or `releases: []`).
    pub releases: Vec<MbRelease>,
    pub cache_response: Option<String>,
}

impl MbLookupOutcome {
    /// First (highest-scoring) match, if any. Convenience for callers
    /// like `:cue-mb` and `:cue-fill` which don't surface a picker.
    pub fn release(&self) -> Option<&MbRelease> {
        self.releases.first()
    }
}

/// Look up the best-matching MusicBrainz release for a disc TOC.
///
/// Database-free for use inside `tokio::spawn`: caller owns cache
/// retrieval (pass the cached JSON body via `cached_response`) and cache
/// storage (write `outcome.cache_response` back if `Some`). On cache hit
/// the function does no HTTP.
///
/// `Ok(MbLookupOutcome { releases: [], .. })` means "no release matched
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
            releases: parse_mb_response_all(&json, n_tracks)?,
            cache_response: None,
        });
    }

    let url = format!(
        "{}?toc={}&inc=artist-credits+isrcs+labels+recordings+release-groups&fmt=json",
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
            releases: Vec::new(),
            cache_response: Some(body),
        });
    }
    if !status.is_success() {
        return Err(format!("MusicBrainz returned HTTP {}", status));
    }

    let releases = parse_mb_response_all(&body, n_tracks)?;
    Ok(MbLookupOutcome {
        releases,
        cache_response: Some(body),
    })
}

/// Parse a MusicBrainz JSON response into all matching releases sorted
/// by descending score. Empty `Vec` when no releases match. `Err(_)` on
/// JSON parse failure or unexpected schema.
///
/// `n_tracks` is the track count implied by the queried TOC; it
/// disambiguates the correct medium for multi-disc releases.
pub fn parse_mb_response_all(
    body: &str,
    n_tracks: usize,
) -> Result<Vec<MbRelease>, String> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(body)
        .map_err(|e| format!("MusicBrainz JSON parse error: {}", e))?;

    // 404 body shape: {"error": "..."} → treat as miss.
    if v.get("error").is_some() {
        return Ok(Vec::new());
    }

    let releases = match v.get("releases").and_then(|r| r.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(Vec::new()),
    };

    // Stable sort by descending score (default 0 when absent).
    let mut indexed: Vec<(i64, &Value)> = releases
        .iter()
        .map(|r| (r.get("score").and_then(|s| s.as_i64()).unwrap_or(0), r))
        .collect();
    indexed.sort_by(|a, b| b.0.cmp(&a.0));

    Ok(indexed
        .into_iter()
        .map(|(_, r)| release_from_json(r, n_tracks))
        .collect())
}

/// Convenience wrapper that returns the highest-scoring release (or
/// `None`). Used by `:cue-mb` and `:cue-fill` which auto-pick.
pub fn parse_mb_response(
    body: &str,
    n_tracks: usize,
) -> Result<Option<MbRelease>, String> {
    Ok(parse_mb_response_all(body, n_tracks)?.into_iter().next())
}

fn release_from_json(rel: &serde_json::Value, n_tracks: usize) -> MbRelease {
    let release_id = rel.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = rel.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let artist = artist_credit_string(rel.get("artist-credit"));
    let artist_id = rel
        .get("artist-credit")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("artist"))
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let year = rel
        .get("date")
        .and_then(|v| v.as_str())
        .and_then(|s| s.get(..4).map(|y| y.to_string()));
    let release_group_id = rel
        .get("release-group")
        .and_then(|rg| rg.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let original_date = rel
        .get("release-group")
        .and_then(|rg| rg.get("first-release-date"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.get(..4).map(|y| y.to_string()));
    let country = rel
        .get("country")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
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

    MbRelease {
        release_id,
        release_group_id,
        artist_id,
        title,
        artist,
        year,
        original_date,
        country,
        catalog,
        barcode,
        tracks,
    }
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
    let track_id = t.get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let recording_id = t.get("recording")
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let artist_id = t.get("artist-credit")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("artist"))
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            t.get("recording")
                .and_then(|r| r.get("artist-credit"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("artist"))
                .and_then(|a| a.get("id"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let isrc = t
        .get("recording")
        .and_then(|r| r.get("isrcs"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(MbTrack {
        position,
        track_id,
        recording_id,
        artist_id,
        title,
        artist,
        isrc,
    })
}

/// Build a review-state from a MusicBrainz release. The same shape as
/// gnudb's so the existing GnudbReview overlay (render + key handling)
/// can show MB results too. The source release is held in
/// `mb_release` so the accept step can populate MB-only fields (ISRC,
/// catalog) that the review UI doesn't expose.
pub fn build_review_state_from_mb(
    release: MbRelease,
    paths: Vec<std::path::PathBuf>,
) -> crate::tui::app::GnudbReviewState {
    use crate::tui::app::{GnudbReviewPage, GnudbReviewState, GnudbReviewTrack, GnudbRowKind};

    let mut tracks = Vec::with_capacity(release.tracks.len());
    for (i, mt) in release.tracks.iter().enumerate() {
        tracks.push(GnudbReviewTrack {
            title: mt.title.clone(),
            artist: if mt.artist.is_empty() { release.artist.clone() } else { mt.artist.clone() },
            track_number: mt.position,
            file_index: i,
        });
    }

    let mut rows: Vec<GnudbRowKind> = Vec::new();
    rows.push(GnudbRowKind::AlbumField("Album"));
    rows.push(GnudbRowKind::AlbumField("Year"));
    rows.push(GnudbRowKind::AlbumField("Genre"));
    for (idx, _) in tracks.iter().enumerate() {
        rows.push(GnudbRowKind::TrackHeader { track_idx: idx });
        rows.push(GnudbRowKind::TrackField { track_idx: idx, field: "Title" });
        rows.push(GnudbRowKind::TrackField { track_idx: idx, field: "Artist" });
    }

    let pages = vec![GnudbReviewPage {
        label: String::new(),
        album: release.title.clone(),
        year: release.year.clone().unwrap_or_default(),
        genre: String::new(), // MB doesn't reliably surface genre.
        tracks,
        rows,
    }];

    GnudbReviewState {
        pages,
        active_page: 0,
        cursor: 0,
        scroll: 0,
        edit_input: None,
        last_click: None,
        origin_matches: None,
        paths,
        mb_release: Some(Box::new(release)),
    }
}

/// Populate a metadata editor state with the MB-only fields that the
/// GnudbReview UI doesn't surface: ISRC, CATALOGNUMBER, MusicBrainz
/// IDs, RELEASECOUNTRY, ORIGINALDATE.
///
/// Called after `populate_editor_from_review` to supplement the
/// user-reviewed Title/Artist/Album/Year/Genre with the rest of MB's
/// data. Empty values are skipped (preserves whatever the file had).
pub fn populate_editor_mb_supplemental(
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

    // Per-track entries.
    let isrc_idx = find_or_create(&mut state.entries, "ISRC", ItemKey::Isrc, n);
    let recording_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_TRACKID", ItemKey::MusicBrainzRecordingId, n,
    );
    let track_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_RELEASETRACKID", ItemKey::MusicBrainzTrackId, n,
    );
    let artist_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_ARTISTID", ItemKey::MusicBrainzArtistId, n,
    );

    // Album-level entries (replicated per file).
    let catalog_idx = find_or_create(
        &mut state.entries, "CATALOGNUMBER", ItemKey::CatalogNumber, n,
    );
    let album_id_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_ALBUMID", ItemKey::MusicBrainzReleaseId, n,
    );
    let album_artist_id_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_ALBUMARTISTID", ItemKey::MusicBrainzReleaseArtistId, n,
    );
    let release_group_idx = find_or_create(
        &mut state.entries, "MUSICBRAINZ_RELEASEGROUPID", ItemKey::MusicBrainzReleaseGroupId, n,
    );
    let original_date_idx = find_or_create(
        &mut state.entries, "ORIGINALDATE", ItemKey::OriginalReleaseDate, n,
    );
    let country_idx = find_or_create(
        &mut state.entries,
        "RELEASECOUNTRY",
        ItemKey::Unknown("RELEASECOUNTRY".to_string()),
        n,
    );

    let catalog_value = release.catalog.as_deref()
        .or(release.barcode.as_deref())
        .unwrap_or("")
        .to_string();

    for i in 0..n {
        // Per-track from the matching MbTrack.
        if let Some(mt) = release.tracks.iter().find(|m| m.position as usize == i + 1) {
            if let Some(s) = mt.isrc.as_deref().filter(|s| !s.is_empty()) {
                state.entries[isrc_idx].per_file_values[i] = s.to_string();
            }
            if let Some(s) = mt.recording_id.as_deref().filter(|s| !s.is_empty()) {
                state.entries[recording_idx].per_file_values[i] = s.to_string();
            }
            if let Some(s) = mt.track_id.as_deref().filter(|s| !s.is_empty()) {
                state.entries[track_idx].per_file_values[i] = s.to_string();
            }
            if let Some(s) = mt.artist_id.as_deref().filter(|s| !s.is_empty()) {
                state.entries[artist_idx].per_file_values[i] = s.to_string();
            }
        }
        // Album-level — replicate across all files.
        if !catalog_value.is_empty() {
            state.entries[catalog_idx].per_file_values[i] = catalog_value.clone();
        }
        if !release.release_id.is_empty() {
            state.entries[album_id_idx].per_file_values[i] = release.release_id.clone();
        }
        if let Some(s) = release.release_group_id.as_deref().filter(|s| !s.is_empty()) {
            state.entries[release_group_idx].per_file_values[i] = s.to_string();
        }
        if let Some(s) = release.artist_id.as_deref().filter(|s| !s.is_empty()) {
            state.entries[album_artist_id_idx].per_file_values[i] = s.to_string();
        }
        if let Some(s) = release.original_date.as_deref().filter(|s| !s.is_empty()) {
            state.entries[original_date_idx].per_file_values[i] = s.to_string();
        }
        if let Some(s) = release.country.as_deref().filter(|s| !s.is_empty()) {
            state.entries[country_idx].per_file_values[i] = s.to_string();
        }
    }

    for idx in [
        isrc_idx, recording_idx, track_idx, artist_idx,
        catalog_idx, album_id_idx, album_artist_id_idx, release_group_idx,
        original_date_idx, country_idx,
    ] {
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

/// Populate a metadata editor state with values from a MusicBrainz release.
/// Track-level fields (TITLE, ARTIST, TRACKNUMBER, ISRC) come from the
/// matching `MbTrack.position`; album-level fields (ALBUM, DATE,
/// CATALOGNUMBER) apply to every track. Existing per-file tag values are
/// only overwritten when the MB value is non-empty — empty MB fields
/// preserve whatever the file currently has.
///
/// Calls `populate_editor_mb_supplemental` for the MB-only fields
/// (IDs, country, original date, composer, lyricist, etc.) and then
/// writes the review-equivalent fields (TITLE/ARTIST/ALBUM/TRACKNUMBER/
/// DATE) on top.
///
/// Mirrors `super::gnudb::populate_editor_from_gnudb`.
pub fn populate_editor_from_mb(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) {
    use lofty::tag::ItemKey;

    populate_editor_mb_supplemental(state, release);

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

    for i in 0..n {
        let mt = release.tracks.iter().find(|m| m.position as usize == i + 1);
        if let Some(mt) = mt {
            if !mt.title.is_empty() {
                state.entries[title_idx].per_file_values[i] = mt.title.clone();
            }
            if !mt.artist.is_empty() {
                state.entries[artist_idx].per_file_values[i] = mt.artist.clone();
            }
        }
        if !release.title.is_empty() {
            state.entries[album_idx].per_file_values[i] = release.title.clone();
        }
        state.entries[tn_idx].per_file_values[i] = (i + 1).to_string();
        if let Some(year) = release.year.as_deref().filter(|s| !s.is_empty()) {
            state.entries[date_idx].per_file_values[i] = year.to_string();
        }
    }

    for idx in [title_idx, artist_idx, album_idx, tn_idx, date_idx] {
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

    /// Build a minimal MbRelease with sane defaults for testing.
    fn rel(id: &str, tracks: Vec<MbTrack>) -> MbRelease {
        MbRelease {
            release_id: id.into(),
            release_group_id: None,
            artist_id: None,
            title: id.into(),
            artist: "Artist".into(),
            year: None,
            original_date: None,
            country: None,
            catalog: None,
            barcode: None,
            tracks,
        }
    }

    /// Build a minimal MbTrack with sane defaults for testing.
    fn trk(position: u32, title: &str, artist: &str, isrc: Option<&str>) -> MbTrack {
        MbTrack {
            position,
            track_id: None,
            recording_id: None,
            artist_id: None,
            title: title.into(),
            artist: artist.into(),
            isrc: isrc.map(String::from),
        }
    }

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
    fn parse_mb_response_all_sorted_by_score_desc() {
        let body = r#"{
            "releases": [
                { "id": "low", "score": 50, "title": "Low", "media": [] },
                { "id": "high", "score": 100, "title": "High", "media": [] },
                { "id": "mid", "score": 75, "title": "Mid", "media": [] }
            ]
        }"#;
        let releases = parse_mb_response_all(body, 0).unwrap();
        assert_eq!(releases.len(), 3);
        assert_eq!(releases[0].release_id, "high");
        assert_eq!(releases[1].release_id, "mid");
        assert_eq!(releases[2].release_id, "low");
    }

    #[test]
    fn parse_mb_response_all_returns_empty_on_no_match() {
        assert!(parse_mb_response_all(r#"{"releases":[]}"#, 0).unwrap().is_empty());
        assert!(parse_mb_response_all(r#"{"error":"not found"}"#, 0).unwrap().is_empty());
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
    fn parse_extracts_release_group_country_and_ids() {
        let body = r#"{
          "releases": [{
            "id": "rid", "title": "Album", "country": "US",
            "artist-credit": [{"name": "A", "artist": {"id": "art-id", "name": "A"}}],
            "release-group": {"id": "rgid", "first-release-date": "1969-08-31"},
            "media": [{
              "tracks": [
                {"id": "tk1", "position": 1, "title": "One",
                 "recording": {"id": "rec1", "isrcs": ["USRC1"]}}
              ]
            }]
          }]
        }"#;
        let r = parse_mb_response(body, 1).unwrap().unwrap();
        assert_eq!(r.release_group_id.as_deref(), Some("rgid"));
        assert_eq!(r.original_date.as_deref(), Some("1969"));
        assert_eq!(r.country.as_deref(), Some("US"));
        assert_eq!(r.artist_id.as_deref(), Some("art-id"));
        assert_eq!(r.tracks[0].track_id.as_deref(), Some("tk1"));
        assert_eq!(r.tracks[0].recording_id.as_deref(), Some("rec1"));
    }

    #[test]
    fn build_review_state_from_mb_populates_pages_and_carries_release() {
        let mut release = rel("rid", vec![
            trk(1, "One", "Artist", Some("USRC1")),
            trk(2, "Two", "", None),
        ]);
        release.title = "Album".into();
        release.year = Some("1971".into());
        release.catalog = Some("CAT-001".into());
        release.barcode = Some("0044007735428".into());
        let paths = vec![
            std::path::PathBuf::from("/x/01.flac"),
            std::path::PathBuf::from("/x/02.flac"),
        ];
        let review = build_review_state_from_mb(release.clone(), paths.clone());
        assert_eq!(review.pages.len(), 1);
        let page = &review.pages[0];
        assert_eq!(page.album, "Album");
        assert_eq!(page.year, "1971");
        assert_eq!(page.tracks.len(), 2);
        assert_eq!(page.tracks[0].title, "One");
        // Empty track artist falls back to release artist.
        assert_eq!(page.tracks[1].artist, "Artist");
        assert_eq!(review.paths, paths);
        assert!(review.mb_release.is_some());
        assert_eq!(review.mb_release.as_ref().unwrap().release_id, "rid");
    }

    fn empty_editor_state(n: usize) -> crate::tui::app::MetadataEditorState {
        use crate::tui::app::{MetadataEditorPhase, MetadataEditorState};
        MetadataEditorState {
            paths: (0..n)
                .map(|i| std::path::PathBuf::from(format!("/tmp/{:02}.flac", i + 1)))
                .collect(),
            entries: Vec::new(),
            cursor: 0, scroll: 0, last_click: None,
            edit_input: None, add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false, deleted: Vec::new(),
            file_labels: (0..n).map(|i| format!("{:02}", i + 1)).collect(),
            detail_field_idx: 0, detail_cursor: 0, detail_scroll: 0, detail_edit: None,
        }
    }

    #[test]
    fn populate_supplemental_writes_isrc_catalog_and_mb_only_fields() {
        let mut state = empty_editor_state(2);
        let mut release = rel("rid", vec![
            trk(1, "T1", "A", Some("USRC1")),
            trk(2, "T2", "A", None),
        ]);
        release.year = Some("1971".into());
        release.catalog = Some("CAT-001".into());
        release.barcode = Some("BAR".into());
        release.release_group_id = Some("rgid".into());
        release.artist_id = Some("artid".into());
        release.original_date = Some("1969".into());
        release.country = Some("US".into());
        release.tracks[0].recording_id = Some("rec1".into());
        release.tracks[0].track_id = Some("tk1".into());
        release.tracks[0].artist_id = Some("artid".into());

        populate_editor_mb_supplemental(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.entries.iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(key))
                .map(|e| e.per_file_values.clone())
                .unwrap_or_default()
        };
        assert_eq!(lookup("ISRC"), vec!["USRC1", ""]);
        assert_eq!(lookup("CATALOGNUMBER"), vec!["CAT-001", "CAT-001"]);
        assert_eq!(lookup("MUSICBRAINZ_ALBUMID"), vec!["rid", "rid"]);
        assert_eq!(lookup("MUSICBRAINZ_RELEASEGROUPID"), vec!["rgid", "rgid"]);
        assert_eq!(lookup("MUSICBRAINZ_ALBUMARTISTID"), vec!["artid", "artid"]);
        assert_eq!(lookup("MUSICBRAINZ_TRACKID"), vec!["rec1", ""]);
        assert_eq!(lookup("MUSICBRAINZ_RELEASETRACKID"), vec!["tk1", ""]);
        assert_eq!(lookup("MUSICBRAINZ_ARTISTID"), vec!["artid", ""]);
        assert_eq!(lookup("ORIGINALDATE"), vec!["1969", "1969"]);
        assert_eq!(lookup("RELEASECOUNTRY"), vec!["US", "US"]);
        // Helper does NOT write Title/Album/Artist/Date.
        assert!(state.entries.iter().find(|e| e.display_key == "TITLE").is_none());
        assert!(state.entries.iter().find(|e| e.display_key == "ALBUM").is_none());
        assert!(state.entries.iter().find(|e| e.display_key == "DATE").is_none());
    }

    #[test]
    fn populate_editor_from_mb_fills_track_and_album_fields() {
        let mut state = empty_editor_state(2);
        let mut release = rel("x", vec![
            trk(1, "Track 1", "Artist", Some("USRC17607839")),
            trk(2, "Track 2", "Artist", None),
        ]);
        release.title = "Album".into();
        release.year = Some("1971".into());
        release.catalog = Some("UICY-94626".into());
        release.barcode = Some("0044007735428".into());
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
