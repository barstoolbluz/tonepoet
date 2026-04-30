//! GNUDB (CDDB) client: disc ID computation, HTTP query, xmcd parsing.
//!
//! Computes a CDDB1 disc ID from track durations, queries gnudb.org via
//! HTTP, and parses the xmcd response into structured metadata.

use std::path::PathBuf;

// ── Data structures ─────────────────────────────────────────────────

/// A match returned by a GNUDB query (one per candidate disc).
#[derive(Debug, Clone)]
pub struct GnudbMatch {
    pub category: String,
    pub disc_id: String,
    pub title: String,
}

/// Parsed xmcd entry from a GNUDB read.
#[derive(Debug, Clone)]
pub struct GnudbEntry {
    pub disc_id: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub genre: String,
    pub tracks: Vec<String>,
}

/// Disc ID computation result.
pub struct DiscIdResult {
    pub disc_id: String,
    pub offsets: Vec<u32>,
    pub total_secs: u32,
    pub n_tracks: usize,
}

// ── Disc ID computation ─────────────────────────────────────────────

/// Compute a CDDB1 disc ID from track durations in seconds.
///
/// Assumes standard Red Book layout: 2-second lead-in (150 frames),
/// 75 frames per second. Returns the 8-hex-digit disc ID plus the
/// frame offsets and total seconds needed for the GNUDB query.
pub fn compute_disc_id(durations_secs: &[f64]) -> DiscIdResult {
    let n = durations_secs.len();

    // Build frame offsets (1/75th second units).
    let mut offsets: Vec<u32> = Vec::with_capacity(n);
    let mut frame = 150u32; // 2-second lead-in
    for &dur in durations_secs {
        offsets.push(frame);
        frame += (dur * 75.0).round() as u32;
    }
    let leadout = frame;
    // CDDB standard: divide THEN subtract (integer truncation matters).
    let total_secs = leadout / 75 - offsets[0] / 75;

    // Checksum: sum of digit-sums of each track's start time in seconds.
    let mut checksum = 0u32;
    for &off in &offsets {
        let mut secs = off / 75;
        while secs > 0 {
            checksum += secs % 10;
            secs /= 10;
        }
    }
    checksum %= 255;

    let disc_id = (checksum << 24) | ((total_secs & 0xFFFF) << 8) | (n as u32 & 0xFF);

    DiscIdResult {
        disc_id: format!("{:08x}", disc_id),
        offsets,
        total_secs,
        n_tracks: n,
    }
}

// ── GNUDB HTTP client ───────────────────────────────────────────────

const GNUDB_BASE: &str = "http://gnudb.gnudb.org/~cddb/cddb.cgi";
const HELLO: &str = "tonepoet+localhost+tonepoet+0.1";

/// Query GNUDB for matching discs.
pub async fn query_gnudb(id: &DiscIdResult) -> Result<Vec<GnudbMatch>, String> {
    let offsets_str: Vec<String> = id.offsets.iter().map(|o| o.to_string()).collect();
    let cmd = format!(
        "cddb+query+{}+{}+{}+{}",
        id.disc_id,
        id.n_tracks,
        offsets_str.join("+"),
        id.total_secs,
    );
    let url = format!("{}?cmd={}&hello={}&proto=6", GNUDB_BASE, cmd, HELLO);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(&url)
        .send()
        .await
        .map_err(|e| format!("GNUDB query failed: {}", e))?;

    let body = resp.text()
        .await
        .map_err(|e| format!("GNUDB response error: {}", e))?;

    parse_query_response(&body)
}

/// Read a specific GNUDB entry by category and disc ID.
pub async fn read_gnudb(category: &str, disc_id: &str) -> Result<GnudbEntry, String> {
    let cmd = format!("cddb+read+{}+{}", category, disc_id);
    let url = format!("{}?cmd={}&hello={}&proto=6", GNUDB_BASE, cmd, HELLO);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(&url)
        .send()
        .await
        .map_err(|e| format!("GNUDB read failed: {}", e))?;

    let body = resp.text()
        .await
        .map_err(|e| format!("GNUDB response error: {}", e))?;

    parse_read_response(&body)
}

// ── Response parsing ────────────────────────────────────────────────

fn parse_query_response(body: &str) -> Result<Vec<GnudbMatch>, String> {
    let first_line = body.lines().next().unwrap_or("");

    if first_line.starts_with("200 ") {
        // Exact match: "200 category discid Artist / Album"
        if let Some(m) = parse_match_line(&first_line[4..]) {
            return Ok(vec![m]);
        }
        return Err("Failed to parse exact match".into());
    }

    if first_line.starts_with("210 ") || first_line.starts_with("211 ") {
        // Multiple matches: lines follow until "."
        let mut matches = Vec::new();
        for line in body.lines().skip(1) {
            let trimmed = line.trim();
            if trimmed == "." { break; }
            if let Some(m) = parse_match_line(trimmed) {
                matches.push(m);
            }
        }
        if matches.is_empty() {
            return Err("No matches in response".into());
        }
        return Ok(matches);
    }

    if first_line.starts_with("202 ") {
        return Err("No match found in GNUDB".into());
    }

    Err(format!("GNUDB error: {}", first_line))
}

/// Parse a single match line: "category discid Artist / Album"
fn parse_match_line(line: &str) -> Option<GnudbMatch> {
    let mut parts = line.splitn(3, ' ');
    let category = parts.next()?.to_string();
    let disc_id = parts.next()?.to_string();
    let title = parts.next()?.to_string();
    Some(GnudbMatch { category, disc_id, title })
}

fn parse_read_response(body: &str) -> Result<GnudbEntry, String> {
    let first_line = body.lines().next().unwrap_or("");
    if !first_line.starts_with("210 ") {
        return Err(format!("GNUDB read error: {}", first_line));
    }

    let mut disc_id = String::new();
    let mut dtitle = String::new();
    let mut year = String::new();
    let mut genre = String::new();
    let mut tracks: Vec<String> = Vec::new();

    // xmcd fields can span multiple lines (concatenated).
    for line in body.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed == "." { break; }
        if trimmed.starts_with('#') { continue; }

        if let Some(val) = trimmed.strip_prefix("DISCID=") {
            disc_id = val.to_string();
        } else if let Some(val) = trimmed.strip_prefix("DTITLE=") {
            dtitle.push_str(val);
        } else if let Some(val) = trimmed.strip_prefix("DYEAR=") {
            year = val.to_string();
        } else if let Some(val) = trimmed.strip_prefix("DGENRE=") {
            genre = val.to_string();
        } else if trimmed.starts_with("TTITLE") {
            // "TTITLE0=Track Title" — extract index and value.
            if let Some(eq_pos) = trimmed.find('=') {
                let idx_str = &trimmed[6..eq_pos];
                let val = &trimmed[eq_pos + 1..];
                if let Ok(idx) = idx_str.parse::<usize>() {
                    // Extend tracks vector if needed.
                    while tracks.len() <= idx {
                        tracks.push(String::new());
                    }
                    // Concatenate (multi-line titles).
                    tracks[idx].push_str(val);
                }
            }
        }
    }

    // Split DTITLE into artist / album.
    let (artist, album) = if let Some(sep) = dtitle.find(" / ") {
        (dtitle[..sep].to_string(), dtitle[sep + 3..].to_string())
    } else {
        (dtitle.clone(), String::new())
    };

    Ok(GnudbEntry {
        disc_id,
        artist,
        album,
        year,
        genre,
        tracks,
    })
}

// ── Collect durations from browse state ─────────────────────────────

/// Populate a metadata editor state with GNUDB entry data.
pub fn populate_editor_from_gnudb(
    state: &mut super::app::MetadataEditorState,
    entry: &GnudbEntry,
) {
    let n = state.paths.len();

    // Helper: find or create an entry by display key.
    fn find_or_create(
        entries: &mut Vec<super::probe::TagEntry>,
        key: &str,
        item_key: lofty::tag::ItemKey,
        n: usize,
    ) -> usize {
        if let Some(i) = entries.iter().position(|e| e.display_key.eq_ignore_ascii_case(key)) {
            return i;
        }
        entries.push(super::probe::TagEntry {
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

    let title_idx = find_or_create(&mut state.entries, "TITLE", lofty::tag::ItemKey::TrackTitle, n);
    let artist_idx = find_or_create(&mut state.entries, "ARTIST", lofty::tag::ItemKey::TrackArtist, n);
    let album_idx = find_or_create(&mut state.entries, "ALBUM", lofty::tag::ItemKey::AlbumTitle, n);
    let tn_idx = find_or_create(&mut state.entries, "TRACKNUMBER", lofty::tag::ItemKey::TrackNumber, n);
    let year_idx = find_or_create(&mut state.entries, "DATE", lofty::tag::ItemKey::Year, n);
    let genre_idx = find_or_create(&mut state.entries, "GENRE", lofty::tag::ItemKey::Genre, n);

    for i in 0..n {
        // Track title.
        if let Some(title) = entry.tracks.get(i) {
            state.entries[title_idx].per_file_values[i] = title.clone();
        }
        // Artist (same for all tracks from GNUDB).
        state.entries[artist_idx].per_file_values[i] = entry.artist.clone();
        // Album (same for all tracks).
        state.entries[album_idx].per_file_values[i] = entry.album.clone();
        // Track number (1-based).
        state.entries[tn_idx].per_file_values[i] = (i + 1).to_string();
        // Year.
        if !entry.year.is_empty() {
            state.entries[year_idx].per_file_values[i] = entry.year.clone();
        }
        // Genre.
        if !entry.genre.is_empty() {
            state.entries[genre_idx].per_file_values[i] = entry.genre.clone();
        }
    }

    // Update merged display values and mixed state.
    for idx in [title_idx, artist_idx, album_idx, tn_idx, year_idx, genre_idx] {
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

/// Populate a single file's tags in the metadata editor from a GNUDB entry.
/// Used by multi-disc flow where each disc populates its own files.
pub fn populate_editor_file(
    state: &mut super::app::MetadataEditorState,
    file_idx: usize,
    entry: &GnudbEntry,
    track_idx: usize,
) {
    let n = state.paths.len();
    if file_idx >= n { return; }

    fn find_or_create(
        entries: &mut Vec<super::probe::TagEntry>,
        key: &str,
        item_key: lofty::tag::ItemKey,
        n: usize,
    ) -> usize {
        if let Some(i) = entries.iter().position(|e| e.display_key.eq_ignore_ascii_case(key)) {
            return i;
        }
        entries.push(super::probe::TagEntry {
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

    let title_idx = find_or_create(&mut state.entries, "TITLE", lofty::tag::ItemKey::TrackTitle, n);
    let artist_idx = find_or_create(&mut state.entries, "ARTIST", lofty::tag::ItemKey::TrackArtist, n);
    let album_idx = find_or_create(&mut state.entries, "ALBUM", lofty::tag::ItemKey::AlbumTitle, n);
    let tn_idx = find_or_create(&mut state.entries, "TRACKNUMBER", lofty::tag::ItemKey::TrackNumber, n);
    let year_idx = find_or_create(&mut state.entries, "DATE", lofty::tag::ItemKey::Year, n);
    let genre_idx = find_or_create(&mut state.entries, "GENRE", lofty::tag::ItemKey::Genre, n);

    if let Some(title) = entry.tracks.get(track_idx) {
        state.entries[title_idx].per_file_values[file_idx] = title.clone();
    }
    state.entries[artist_idx].per_file_values[file_idx] = entry.artist.clone();
    state.entries[album_idx].per_file_values[file_idx] = entry.album.clone();
    state.entries[tn_idx].per_file_values[file_idx] = (track_idx + 1).to_string();
    if !entry.year.is_empty() {
        state.entries[year_idx].per_file_values[file_idx] = entry.year.clone();
    }
    if !entry.genre.is_empty() {
        state.entries[genre_idx].per_file_values[file_idx] = entry.genre.clone();
    }

    // Update merged display values for all modified fields.
    for idx in [title_idx, artist_idx, album_idx, tn_idx, year_idx, genre_idx] {
        let e = &mut state.entries[idx];
        let all_same = e.per_file_values.windows(2).all(|w| w[0] == w[1]);
        e.is_mixed = !all_same && n > 1;
        e.value = if e.is_mixed {
            "<multiple values>".to_string()
        } else {
            e.per_file_values.first().cloned().unwrap_or_default()
        };
    }
}

// ── Review state builders ────────────────────────────────────────────

/// Build a GnudbReviewState from a single GNUDB entry.
pub fn build_review_state(
    entry: &GnudbEntry,
    paths: Vec<PathBuf>,
) -> super::app::GnudbReviewState {
    use super::app::*;

    let mut tracks = Vec::new();
    for (i, title) in entry.tracks.iter().enumerate() {
        tracks.push(GnudbReviewTrack {
            title: title.clone(),
            artist: entry.artist.clone(),
            track_number: (i + 1) as u32,
            file_index: i,
        });
    }

    let rows = build_page_rows(&tracks);
    let pages = vec![GnudbReviewPage {
        label: String::new(),
        album: entry.album.clone(),
        year: entry.year.clone(),
        genre: entry.genre.clone(),
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
    }
}

/// Build a GnudbReviewState from multiple disc entries.
pub fn build_multi_disc_review_state(
    entries: &[(String, GnudbEntry, Vec<PathBuf>)],
) -> super::app::GnudbReviewState {
    use super::app::*;

    let mut all_paths = Vec::new();
    let mut pages = Vec::new();

    for (label, entry, group_paths) in entries {
        let base_file_idx = all_paths.len();
        all_paths.extend(group_paths.iter().cloned());

        let mut tracks = Vec::new();
        for (i, title) in entry.tracks.iter().enumerate() {
            tracks.push(GnudbReviewTrack {
                title: title.clone(),
                artist: entry.artist.clone(),
                track_number: (i + 1) as u32,
                file_index: base_file_idx + i,
            });
        }

        let rows = build_page_rows(&tracks);
        pages.push(GnudbReviewPage {
            label: label.clone(),
            album: entry.album.clone(),
            year: entry.year.clone(),
            genre: entry.genre.clone(),
            tracks,
            rows,
        });
    }

    GnudbReviewState {
        pages,
        active_page: 0,
        cursor: 0,
        scroll: 0,
        edit_input: None,
        last_click: None,
        origin_matches: None,
        paths: all_paths,
    }
}

/// Build the flattened row map for a single page.
fn build_page_rows(tracks: &[super::app::GnudbReviewTrack]) -> Vec<super::app::GnudbRowKind> {
    use super::app::GnudbRowKind;

    let mut rows = vec![
        GnudbRowKind::AlbumField("Album"),
        GnudbRowKind::AlbumField("Year"),
        GnudbRowKind::AlbumField("Genre"),
    ];

    for (ti, _track) in tracks.iter().enumerate() {
        rows.push(GnudbRowKind::TrackHeader { track_idx: ti });
        rows.push(GnudbRowKind::TrackField { track_idx: ti, field: "Title" });
        rows.push(GnudbRowKind::TrackField { track_idx: ti, field: "Artist" });
    }

    rows
}

/// Populate a metadata editor from the reviewed GNUDB state.
pub fn populate_editor_from_review(
    state: &mut super::app::MetadataEditorState,
    review: &super::app::GnudbReviewState,
) {
    let n = state.paths.len();

    fn find_or_create(
        entries: &mut Vec<super::probe::TagEntry>,
        key: &str,
        item_key: lofty::tag::ItemKey,
        n: usize,
    ) -> usize {
        if let Some(i) = entries.iter().position(|e| e.display_key.eq_ignore_ascii_case(key)) {
            return i;
        }
        entries.push(super::probe::TagEntry {
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

    let title_idx = find_or_create(&mut state.entries, "TITLE", lofty::tag::ItemKey::TrackTitle, n);
    let artist_idx = find_or_create(&mut state.entries, "ARTIST", lofty::tag::ItemKey::TrackArtist, n);
    let album_idx = find_or_create(&mut state.entries, "ALBUM", lofty::tag::ItemKey::AlbumTitle, n);
    let tn_idx = find_or_create(&mut state.entries, "TRACKNUMBER", lofty::tag::ItemKey::TrackNumber, n);
    let year_idx = find_or_create(&mut state.entries, "DATE", lofty::tag::ItemKey::Year, n);
    let genre_idx = find_or_create(&mut state.entries, "GENRE", lofty::tag::ItemKey::Genre, n);

    for page in &review.pages {
        for track in &page.tracks {
            let i = track.file_index;
            if i >= n { continue; }
            state.entries[title_idx].per_file_values[i] = track.title.clone();
            state.entries[artist_idx].per_file_values[i] = track.artist.clone();
            state.entries[album_idx].per_file_values[i] = page.album.clone();
            state.entries[tn_idx].per_file_values[i] = track.track_number.to_string();
            if !page.year.is_empty() {
                state.entries[year_idx].per_file_values[i] = page.year.clone();
            }
            if !page.genre.is_empty() {
                state.entries[genre_idx].per_file_values[i] = page.genre.clone();
            }
        }
    }

    for idx in [title_idx, artist_idx, album_idx, tn_idx, year_idx, genre_idx] {
        let e = &mut state.entries[idx];
        let all_same = e.per_file_values.windows(2).all(|w| w[0] == w[1]);
        e.is_mixed = !all_same && n > 1;
        e.value = if e.is_mixed {
            "<multiple values>".to_string()
        } else {
            e.per_file_values.first().cloned().unwrap_or_default()
        };
    }

    state.dirty = true;
}

/// Group audio file paths by parent directory (disc detection).
///
/// Returns `(disc_label, paths)` pairs sorted by disc number. If all
/// files share the same parent directory, returns a single group with
/// an empty label. Multi-disc layouts produce one group per subdirectory
/// (e.g., "Disc 1", "Disc 2").
pub fn group_by_disc(paths: &[PathBuf]) -> Vec<(String, Vec<PathBuf>)> {
    use std::collections::BTreeMap;

    // Group by parent directory path.
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in paths {
        let parent = path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
        groups.entry(parent).or_default().push(path.clone());
    }

    if groups.len() <= 1 {
        // Single directory — return one group with empty label.
        let paths = groups.into_values().next().unwrap_or_default();
        return vec![("".to_string(), paths)];
    }

    // Multiple directories — sort by disc number and label.
    let mut result: Vec<(u32, String, Vec<PathBuf>)> = groups.into_iter().map(|(dir, files)| {
        // Use the first file to detect disc number.
        let disc_num = files.first()
            .map(|p| super::probe::extract_disc_from_path(p))
            .unwrap_or(1);
        let label = dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("Disc {}", disc_num));
        (disc_num, label, files)
    }).collect();

    result.sort_by_key(|(num, _, _)| *num);
    result.into_iter().map(|(_, label, files)| (label, files)).collect()
}

// ── CUE → review state builders ─────────────────────────────────────

/// Build a GnudbReviewState from a parsed CUE sheet.
pub fn build_review_state_from_cue(
    sheet: &super::cue_parser::CueSheet,
    paths: Vec<PathBuf>,
) -> super::app::GnudbReviewState {
    use super::app::*;

    let album_performer = sheet.performer.clone().unwrap_or_default();

    let mut tracks = Vec::new();
    for (i, ct) in sheet.tracks.iter().enumerate() {
        tracks.push(GnudbReviewTrack {
            title: ct.title.clone().unwrap_or_default(),
            artist: ct.performer.clone().unwrap_or_else(|| album_performer.clone()),
            track_number: ct.number,
            file_index: i,
        });
    }

    let rows = build_page_rows(&tracks);
    let pages = vec![GnudbReviewPage {
        label: String::new(),
        album: sheet.title.clone().unwrap_or_default(),
        year: sheet.date.clone().unwrap_or_default(),
        genre: sheet.genre.clone().unwrap_or_default(),
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
    }
}

/// Build a GnudbReviewState from multiple disc CUE sheets.
pub fn build_multi_disc_review_state_from_cue(
    entries: &[(String, super::cue_parser::CueSheet, Vec<PathBuf>)],
) -> super::app::GnudbReviewState {
    use super::app::*;

    let mut all_paths = Vec::new();
    let mut pages = Vec::new();

    for (label, sheet, group_paths) in entries {
        let base_file_idx = all_paths.len();
        all_paths.extend(group_paths.iter().cloned());

        let album_performer = sheet.performer.clone().unwrap_or_default();
        let mut tracks = Vec::new();
        for (i, ct) in sheet.tracks.iter().enumerate() {
            tracks.push(GnudbReviewTrack {
                title: ct.title.clone().unwrap_or_default(),
                artist: ct.performer.clone().unwrap_or_else(|| album_performer.clone()),
                track_number: ct.number,
                file_index: base_file_idx + i,
            });
        }

        let rows = build_page_rows(&tracks);
        pages.push(GnudbReviewPage {
            label: label.clone(),
            album: sheet.title.clone().unwrap_or_default(),
            year: sheet.date.clone().unwrap_or_default(),
            genre: sheet.genre.clone().unwrap_or_default(),
            tracks,
            rows,
        });
    }

    GnudbReviewState {
        pages,
        active_page: 0,
        cursor: 0,
        scroll: 0,
        edit_input: None,
        last_click: None,
        origin_matches: None,
        paths: all_paths,
    }
}

/// Find a .cue file in a directory. Returns the first one found
/// (sorted alphabetically for determinism).
pub fn find_cue_in_dir(dir: &std::path::Path) -> Option<PathBuf> {
    let mut cues: Vec<PathBuf> = std::fs::read_dir(dir).ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("cue")).unwrap_or(false) {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    cues.sort();
    cues.into_iter().next()
}

/// Collect track durations for audio files. Checks the probe cache first,
/// falls back to direct ffmpeg probe for files not yet cached.
pub fn collect_durations(
    paths: &[PathBuf],
    probe_cache: &std::collections::HashMap<PathBuf, Option<std::sync::Arc<super::browse::CachedInfo>>>,
) -> Vec<f64> {
    let mut durations = Vec::new();
    for path in paths {
        if let Some(Some(cached)) = probe_cache.get(path) {
            durations.push(cached.source.duration_secs);
        } else {
            // Not in cache — probe directly (synchronous, but fast).
            // Try ffmpeg first, fall back to format-specific tools
            // (e.g., wvunpack for older WavPack files ffmpeg can't read).
            match super::probe::probe_audio(path) {
                Ok(info) => durations.push(info.duration_secs),
                Err(_) => {
                    // Fallback: use probe_sample_count which has
                    // format-specific fallbacks (wvunpack, etc.).
                    if let Ok((samples, sr)) = super::accuraterip::probe_sample_count(path) {
                        durations.push(samples as f64 / sr as f64);
                    }
                    // If both fail, skip — durations vec will be short,
                    // triggering the "some durations missing" error.
                }
            }
        }
    }
    durations
}
