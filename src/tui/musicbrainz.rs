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
    /// Track length in milliseconds, when MB exposes it. Required for
    /// generating an embedded CUESHEET tag on single-image rips.
    pub length_ms: Option<u32>,
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
    // Length in ms, preferring the track-level value (the disc-encoded
    // length); fall back to recording.length when the track doesn't
    // carry its own.
    let length_ms = t.get("length")
        .and_then(|v| v.as_u64())
        .or_else(|| t.get("recording")
            .and_then(|r| r.get("length"))
            .and_then(|v| v.as_u64()))
        .map(|n| n as u32);

    Some(MbTrack {
        position,
        track_id,
        recording_id,
        artist_id,
        title,
        artist,
        isrc,
        length_ms,
    })
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
    // Single-image rip: one file representing a multi-track release.
    // Track-level IDs (ISRC, MUSICBRAINZ_TRACKID, MUSICBRAINZ_RECORDINGID,
    // per-track ARTISTID) don't apply when the file IS the album, so
    // skip creating those entries. Album-level IDs (ALBUMID, etc.)
    // still get written.
    let single_image = n == 1 && release.tracks.len() > 1;

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
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
        entries.len() - 1
    }

    // Per-track presence pre-pass: only create the entry when at least
    // one track in the release has data for that field. Tracks with no
    // data leave their per-file value as the empty string (matching the
    // pre-existing "MB silent on this track" behavior). For
    // single-image rips, skip these entirely (track-level IDs are
    // meaningless when the file is the whole album).
    let any_isrc = !single_image && release.tracks.iter()
        .any(|t| t.isrc.as_deref().is_some_and(|s| !s.is_empty()));
    let any_recording = !single_image && release.tracks.iter()
        .any(|t| t.recording_id.as_deref().is_some_and(|s| !s.is_empty()));
    let any_track = !single_image && release.tracks.iter()
        .any(|t| t.track_id.as_deref().is_some_and(|s| !s.is_empty()));
    let any_track_artist = !single_image && release.tracks.iter()
        .any(|t| t.artist_id.as_deref().is_some_and(|s| !s.is_empty()));

    let isrc_idx = if any_isrc {
        Some(find_or_create(&mut state.entries, "ISRC", ItemKey::Isrc, n))
    } else { None };
    let recording_idx = if any_recording {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_TRACKID", ItemKey::MusicBrainzRecordingId, n,
        ))
    } else { None };
    let track_idx = if any_track {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_RELEASETRACKID", ItemKey::MusicBrainzTrackId, n,
        ))
    } else { None };
    let artist_idx = if any_track_artist {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_ARTISTID", ItemKey::MusicBrainzArtistId, n,
        ))
    } else { None };

    // Album-level — gate each entry on MB actually having a value.
    let catalog_value = release.catalog.as_deref()
        .or(release.barcode.as_deref())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let catalog_idx = if catalog_value.is_some() {
        Some(find_or_create(&mut state.entries, "CATALOGNUMBER", ItemKey::CatalogNumber, n))
    } else { None };
    let album_id_idx = if !release.release_id.is_empty() {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_ALBUMID", ItemKey::MusicBrainzReleaseId, n,
        ))
    } else { None };
    let album_artist_id_idx = if release.artist_id.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_ALBUMARTISTID", ItemKey::MusicBrainzReleaseArtistId, n,
        ))
    } else { None };
    let release_group_idx = if release.release_group_id.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.entries, "MUSICBRAINZ_RELEASEGROUPID", ItemKey::MusicBrainzReleaseGroupId, n,
        ))
    } else { None };
    let original_date_idx = if release.original_date.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.entries, "ORIGINALDATE", ItemKey::OriginalReleaseDate, n,
        ))
    } else { None };
    let country_idx = if release.country.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.entries,
            "RELEASECOUNTRY",
            ItemKey::Unknown("RELEASECOUNTRY".to_string()),
            n,
        ))
    } else { None };

    for i in 0..n {
        if let Some(mt) = release.tracks.iter().find(|m| m.position as usize == i + 1) {
            if let (Some(idx), Some(s)) = (
                isrc_idx, mt.isrc.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.entries[idx].per_file_values[i] = s.to_string();
            }
            if let (Some(idx), Some(s)) = (
                recording_idx, mt.recording_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.entries[idx].per_file_values[i] = s.to_string();
            }
            if let (Some(idx), Some(s)) = (
                track_idx, mt.track_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.entries[idx].per_file_values[i] = s.to_string();
            }
            if let (Some(idx), Some(s)) = (
                artist_idx, mt.artist_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.entries[idx].per_file_values[i] = s.to_string();
            }
        }
        if let (Some(idx), Some(s)) = (catalog_idx, catalog_value.as_deref()) {
            state.entries[idx].per_file_values[i] = s.to_string();
        }
        if let Some(idx) = album_id_idx {
            state.entries[idx].per_file_values[i] = release.release_id.clone();
        }
        if let (Some(idx), Some(s)) = (
            album_artist_id_idx, release.artist_id.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            release_group_idx, release.release_group_id.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            original_date_idx, release.original_date.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            country_idx, release.country.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.entries[idx].per_file_values[i] = s.to_string();
        }
    }

    for idx in [
        isrc_idx, recording_idx, track_idx, artist_idx,
        catalog_idx, album_id_idx, album_artist_id_idx, release_group_idx,
        original_date_idx, country_idx,
    ].iter().filter_map(|x| *x) {
        recompute_and_stamp_mb_proposed(&mut state.entries[idx], n);
    }

    crate::tui::probe::sort_entries_standard_first(&mut state.entries);
    state.dirty = true;
}

/// Recompute `value` / `is_mixed` for an entry after a populate touched
/// its `per_file_values`, and stamp `mb_proposed_value` /
/// `mb_proposed_per_file` so the editor can show a `[revert]` /
/// `[use MB]` toggle pill.
///
/// Stamps only when the resulting value actually differs from the
/// pre-populate `original` — fields where MB happened to match what the
/// file already had don't need a toggle.
fn recompute_and_stamp_mb_proposed(
    entry: &mut crate::tui::probe::TagEntry,
    n: usize,
) {
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    entry.is_mixed = !all_same && n > 1;
    entry.value = if entry.is_mixed {
        "<multiple values>".to_string()
    } else {
        entry.per_file_values.first().cloned().unwrap_or_default()
    };

    if entry.value != entry.original
        || entry.per_file_values != entry.per_file_originals
    {
        entry.mb_proposed_value = Some(entry.value.clone());
        entry.mb_proposed_per_file = Some(entry.per_file_values.clone());
    }
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
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
        entries.len() - 1
    }

    // Per-track presence pre-pass: only create entries when at least
    // one track in the release has data for that field.
    let any_title = release.tracks.iter().any(|t| !t.title.is_empty());
    let any_artist = release.tracks.iter().any(|t| !t.artist.is_empty());

    let title_idx = if any_title {
        Some(find_or_create(&mut state.entries, "TITLE", ItemKey::TrackTitle, n))
    } else { None };
    let artist_idx = if any_artist {
        Some(find_or_create(&mut state.entries, "ARTIST", ItemKey::TrackArtist, n))
    } else { None };
    let album_idx = if !release.title.is_empty() {
        Some(find_or_create(&mut state.entries, "ALBUM", ItemKey::AlbumTitle, n))
    } else { None };
    // TRACKNUMBER is always 1-based-by-file-position, computed locally —
    // doesn't depend on MB content. Always create.
    let tn_idx = find_or_create(&mut state.entries, "TRACKNUMBER", ItemKey::TrackNumber, n);
    let date_idx = if release.year.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(&mut state.entries, "DATE", ItemKey::Year, n))
    } else { None };

    // Single-image rip detection: one file, multi-track release. The
    // file represents the whole album, so TITLE/ARTIST should carry
    // the album-level values rather than track 1's. (Foobar2000 / EAC
    // write the album title here for the same reason.)
    let single_image = n == 1 && release.tracks.len() > 1;

    for i in 0..n {
        let mt = release.tracks.iter().find(|m| m.position as usize == i + 1);
        if single_image {
            if let Some(idx) = title_idx {
                state.entries[idx].per_file_values[i] = release.title.clone();
            }
            if let (Some(idx), false) = (artist_idx, release.artist.is_empty()) {
                state.entries[idx].per_file_values[i] = release.artist.clone();
            }
        } else if let Some(mt) = mt {
            if let (Some(idx), false) = (title_idx, mt.title.is_empty()) {
                state.entries[idx].per_file_values[i] = mt.title.clone();
            }
            if let (Some(idx), false) = (artist_idx, mt.artist.is_empty()) {
                state.entries[idx].per_file_values[i] = mt.artist.clone();
            }
        }
        if let Some(idx) = album_idx {
            state.entries[idx].per_file_values[i] = release.title.clone();
        }
        state.entries[tn_idx].per_file_values[i] = (i + 1).to_string();
        if let (Some(idx), Some(year)) = (
            date_idx, release.year.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.entries[idx].per_file_values[i] = year.to_string();
        }
    }

    for idx in [title_idx, artist_idx, album_idx, Some(tn_idx), date_idx]
        .iter().filter_map(|x| *x)
    {
        recompute_and_stamp_mb_proposed(&mut state.entries[idx], n);
    }

    crate::tui::probe::sort_entries_standard_first(&mut state.entries);
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
            length_ms: None,
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
    fn parse_mb_extracts_track_length_ms() {
        // Track-level length wins; recording.length is a fallback.
        let body = r#"{
            "releases": [{
                "id": "rid", "score": 100, "title": "Album",
                "media": [{
                    "tracks": [
                        { "position": 1, "title": "A", "length": 240000 },
                        { "position": 2, "title": "B",
                          "recording": { "length": 180000 } },
                        { "position": 3, "title": "C" }
                    ]
                }]
            }]
        }"#;
        let r = parse_mb_response(body, 3).unwrap().unwrap();
        assert_eq!(r.tracks[0].length_ms, Some(240000), "track-level length");
        assert_eq!(r.tracks[1].length_ms, Some(180000), "recording-level fallback");
        assert_eq!(r.tracks[2].length_ms, None, "no length means None");
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
    fn populate_sorts_entries_with_mb_keys_in_logical_positions() {
        let mut state = empty_editor_state(2);
        let mut release = rel("rid", vec![
            trk(1, "T1", "A", Some("USRC1")),
            trk(2, "T2", "A", Some("USRC2")),
        ]);
        release.title = "Album".into();
        release.year = Some("1971".into());
        release.original_date = Some("1969".into());
        release.barcode = Some("0044007735428".into());
        release.country = Some("US".into());
        release.release_group_id = Some("rgid".into());
        release.tracks[0].recording_id = Some("rec1".into());
        release.tracks[1].recording_id = Some("rec2".into());

        populate_editor_from_mb(&mut state, &release);

        let pos = |key: &str| -> Option<usize> {
            state.entries.iter().position(|e| e.display_key == key)
        };

        // STANDARD_KEY_ORDER positions enforced.
        assert!(pos("TITLE") < pos("ARTIST"));
        assert!(pos("ARTIST") < pos("ALBUM"));
        assert!(pos("ALBUM") < pos("DATE"));
        assert!(pos("DATE") < pos("ORIGINALDATE"));
        assert!(pos("ORIGINALDATE") < pos("TRACKNUMBER"));
        assert!(pos("TRACKNUMBER") < pos("CATALOGNUMBER"));
        assert!(pos("CATALOGNUMBER") < pos("RELEASECOUNTRY"));
        assert!(pos("RELEASECOUNTRY") < pos("ISRC"));
        assert!(pos("ISRC") < pos("MUSICBRAINZ_ALBUMID"));
        assert!(pos("MUSICBRAINZ_ALBUMID") < pos("MUSICBRAINZ_RELEASEGROUPID"));
        assert!(pos("MUSICBRAINZ_RELEASEGROUPID") < pos("MUSICBRAINZ_TRACKID"));
    }

    #[test]
    fn populate_supplemental_skips_entries_for_fields_mb_didnt_supply() {
        let mut state = empty_editor_state(2);
        // MB returns release_id only — no catalog/barcode/country/IDs/etc.
        let release = rel("rid", vec![
            trk(1, "T1", "A", None),
            trk(2, "T2", "A", None),
        ]);
        populate_editor_mb_supplemental(&mut state, &release);

        // ALBUMID gets created because release_id is non-empty.
        assert!(state.entries.iter().any(|e| e.display_key == "MUSICBRAINZ_ALBUMID"));
        // None of these MB-only entries should exist (MB had nothing).
        for absent in [
            "ISRC", "MUSICBRAINZ_TRACKID", "MUSICBRAINZ_RELEASETRACKID",
            "MUSICBRAINZ_ARTISTID", "MUSICBRAINZ_ALBUMARTISTID",
            "MUSICBRAINZ_RELEASEGROUPID", "ORIGINALDATE", "RELEASECOUNTRY",
            "CATALOGNUMBER",
        ] {
            assert!(
                state.entries.iter().find(|e| e.display_key == absent).is_none(),
                "expected no {} entry but found one", absent,
            );
        }
    }

    #[test]
    fn populate_stamps_mb_proposed_for_changed_fields() {
        let mut state = empty_editor_state(2);
        let mut release = rel("rid", vec![
            trk(1, "Track 1", "Artist", Some("USRC1")),
            trk(2, "Track 2", "Artist", None),
        ]);
        release.title = "Album".into();
        release.year = Some("1971".into());
        populate_editor_from_mb(&mut state, &release);

        let title_entry = state.entries.iter()
            .find(|e| e.display_key == "TITLE").expect("TITLE entry");
        assert!(title_entry.mb_proposed_value.is_some());
        assert_eq!(
            title_entry.mb_proposed_per_file.as_ref().unwrap(),
            &vec!["Track 1".to_string(), "Track 2".to_string()],
        );
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
    fn populate_editor_from_mb_single_image_uses_album_title() {
        // Single-image rip: one file, multi-track release. TITLE should
        // be the album title, NOT track 1's title (which is what the
        // pre-fix code did). Track-level IDs (ISRC, MUSICBRAINZ_TRACKID,
        // MUSICBRAINZ_ARTISTID) should not appear at all.
        let mut state = empty_editor_state(1);
        let mut release = rel("rid", vec![
            {
                let mut t = trk(1, "Lead-off Track", "Artist A", Some("USRC17607839"));
                t.recording_id = Some("rec1".into());
                t.track_id = Some("tk1".into());
                t.artist_id = Some("artid1".into());
                t
            },
            trk(2, "Second Track", "Artist B", None),
            trk(3, "Third Track", "Artist C", None),
        ]);
        release.title = "Whole Album".into();
        release.artist = "Album Artist".into();
        release.year = Some("1970".into());
        populate_editor_from_mb(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.entries.iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(key))
                .map(|e| e.per_file_values.clone())
                .unwrap_or_default()
        };
        assert_eq!(lookup("TITLE"), vec!["Whole Album"], "TITLE must be album, not track 1");
        assert_eq!(lookup("ARTIST"), vec!["Album Artist"], "ARTIST must be album-level");
        assert_eq!(lookup("ALBUM"), vec!["Whole Album"]);
        assert_eq!(lookup("DATE"), vec!["1970"]);
        // Track-level IDs must NOT have been created.
        assert!(state.entries.iter().find(|e| e.display_key == "ISRC").is_none(),
            "ISRC must not be written for single-image");
        assert!(state.entries.iter().find(|e| e.display_key == "MUSICBRAINZ_TRACKID").is_none(),
            "MUSICBRAINZ_TRACKID must not be written for single-image");
        assert!(state.entries.iter().find(|e| e.display_key == "MUSICBRAINZ_RELEASETRACKID").is_none());
        assert!(state.entries.iter().find(|e| e.display_key == "MUSICBRAINZ_ARTISTID").is_none());
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
