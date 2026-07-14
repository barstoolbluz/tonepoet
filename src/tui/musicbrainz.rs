//! MusicBrainz integration. Two query paths today:
//!
//! - **Disc-TOC lookup** (`/ws/2/discid/-?toc=...`): used by `:cue-mb`
//!   (overwrite) and `:cue-fill` (enrich) on CD-rip editors.
//! - **Text/release search** (`/ws/2/release/?query=...`): used by
//!   `:tags-mb` on SACD editors (Phase C). Built on top of the Lucene
//!   escape helper and a two-step search → release-detail fetch.
//!
//! TOC values are 1-based: `first+last+leadout+offset_1+...+offset_N`,
//! all in absolute sectors (with the standard 150-frame leadin).
//!
//! MusicBrainz rate-limit policy: 1 request/sec per IP, identified by a
//! meaningful User-Agent. We comply via:
//! 1. A version-encoded UA string (`USER_AGENT` below).
//! 2. A **shared global rate limiter** (`mb_acquire`) that every
//!    `/ws/2/*` call passes through. Single global token, not per
//!    endpoint — MB's quota is per-IP across the whole namespace.
//! 3. An SQLite cache table for TOC lookups (and, Phase B, search
//!    results) so repeated queries don't hit the network at all.

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
    /// Number of media (discs) in the release. Used by the
    /// single-image CUESHEET-embed gate to skip multi-disc releases
    /// (which can't be unambiguously embedded into one file).
    pub disc_count: usize,
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
const MB_RELEASE_BASE: &str = "https://musicbrainz.org/ws/2/release/";
const USER_AGENT: &str = concat!(
    "tonepoet/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/barstoolbluz/tonepoet)"
);

/// MusicBrainz rate-limit interval per the public policy: 1 request
/// per second per IP across the whole `/ws/2/*` namespace.
const MB_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Shared global rate limiter for every `/ws/2/*` call. Stores the
/// `tokio::time::Instant` at which the next request is allowed to
/// fire; acquires hold the lock across `sleep` so concurrent callers
/// queue behind each other deterministically rather than racing the
/// clock.
///
/// This must be one token, not per-endpoint — MB's quota is per-IP
/// across the whole namespace. Sharing across `lookup_release_by_toc`,
/// `search_releases_by_query`, and the per-release detail fetch is
/// the whole point.
///
/// Uses `tokio::time::Instant` (not `std::time::Instant`) so that
/// `tokio::test(start_paused = true)` runtime can drive virtual time
/// in tests without real-time sleeping.
///
/// **Test isolation:** this static persists across the process, so
/// tests that exercise `mb_acquire` under paused time must reset
/// `*MB_NEXT_ALLOWED.lock().await = tokio::time::Instant::now()`
/// at the top of the test, or a stale future-dated value from a
/// prior test will skew the timing. See
/// `mb_rate_limiter_serializes_five_calls_across_four_seconds` for
/// the pattern.
static MB_NEXT_ALLOWED: once_cell::sync::Lazy<tokio::sync::Mutex<tokio::time::Instant>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(tokio::time::Instant::now()));

/// Acquire the right to fire a single MB request. Awaits until the
/// shared rate-limit window opens, then writes the next-allowed
/// instant before returning. Holds the lock across the sleep so
/// concurrent callers serialize through the same gate.
///
/// Cancel safety: if the future is dropped mid-sleep, the lock is
/// released without updating `MB_NEXT_ALLOWED` — the cancelled caller
/// hadn't yet committed to a slot, so the next caller sees the
/// previous occupant's deadline. Correct behavior.
pub(super) async fn mb_acquire() {
    let mut next = MB_NEXT_ALLOWED.lock().await;
    let now = tokio::time::Instant::now();
    if let Some(wait) = next.checked_duration_since(now) {
        tokio::time::sleep(wait).await;
    }
    *next = tokio::time::Instant::now() + MB_MIN_INTERVAL;
}

/// Backslash-escape Lucene metacharacters for use inside MusicBrainz's
/// `/ws/2/release/?query=` parameter (Phase B text search).
///
/// Lucene's reserved set per the query-parser grammar is:
/// `+ - && || ! ( ) { } [ ] ^ " ~ * ? : \ /`
///
/// Single-character operators get a single backslash prefix. The
/// boolean operators `&&` and `||` are doubled-character and need
/// **both** characters escaped (`\&\&`, `\|\|`) — lone `&` and `|` are
/// NOT reserved and pass through. Apostrophe is also NOT reserved.
///
/// The helper conservatively escapes the **full** reserved set,
/// suitable for both quoted-value contexts and bare-term contexts.
/// Most callers will wrap the result in quotes
/// (`format!("artist:\"{}\"", lucene_escape(s))`); inside quotes,
/// Lucene treats characters like `(`, `[`, `:` as literal anyway, so
/// the extra backslashes are harmless. Empirically verified against
/// MB's parser: `release:"...(remix)..."` and
/// `release:"...\(remix\)..."` return identical result sets.
pub(super) fn lucene_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Two-char boolean operators.
        let next = chars.get(i + 1).copied();
        if (c == '&' && next == Some('&')) || (c == '|' && next == Some('|')) {
            out.push('\\');
            out.push(c);
            out.push('\\');
            out.push(c);
            i += 2;
            continue;
        }
        // Single-char reserved set.
        if matches!(
            c,
            '+' | '-'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
        i += 1;
    }
    out
}

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

/// Red Book minimum track length. A track shorter than this cannot exist on
/// a real CD, so it can never participate in a legitimate CD TOC — which
/// makes it the principled threshold for identifying spurious stub "tracks"
/// (menu fragments, 7KB AOB stubs) in TOCs synthesized from DVD-Audio /
/// DVD-Video / SACD durations.
pub const CD_MIN_TRACK_FRAMES: u32 = 4 * 75;

/// One candidate TOC in the stub-drop cascade, plus the mapping back to the
/// source track list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocCandidate {
    /// `[off1, …, offN, leadout]` absolute CD-frame offsets (off1 = 150).
    pub sectors: Vec<u32>,
    /// Source track indices (0-based) that survive in this candidate, in
    /// order. `kept_indices[k]` is the source track that MB track `k`
    /// describes when this candidate matches.
    pub kept_indices: Vec<usize>,
    /// Short human-readable stage label for status lines.
    pub label: &'static str,
}

impl TocCandidate {
    /// Wrap an exact, trusted TOC (real CD rip offsets) as the sole
    /// candidate: no tracks dropped, no cascade.
    pub fn exact(sectors: Vec<u32>) -> Self {
        let n = sectors.len().saturating_sub(1);
        TocCandidate {
            sectors,
            kept_indices: (0..n).collect(),
            label: "as-is",
        }
    }

    /// Source track indices (0-based) excluded by this candidate.
    pub fn dropped_indices(&self, total: usize) -> Vec<usize> {
        (0..total)
            .filter(|i| !self.kept_indices.contains(i))
            .collect()
    }
}

fn sectors_from_kept_frames(frames: &[u32], kept: &[usize]) -> Vec<u32> {
    let mut sectors = Vec::with_capacity(kept.len() + 1);
    let mut cur: u32 = 150;
    sectors.push(cur);
    for &i in kept {
        cur = cur.saturating_add(frames[i]);
        sectors.push(cur);
    }
    sectors
}

/// Build the cascading stub-drop TOC candidates from per-track CD-frame
/// counts (the caller performs its own duration→frame rounding so the
/// "as-is" candidate is byte-identical to the historical single TOC and
/// existing cache entries keep hitting).
///
/// Stage order — try the untouched TOC first, then progressively drop
/// sub-Red-Book stub tracks (first/last/both edges, then everywhere):
/// discs mastered with spurious fragment tracks (DVD-A menu stubs, DVD-V
/// credit chapters) otherwise synthesize a TOC with a track count no real
/// release has, and the MB fuzzy lookup can never match. Candidates are
/// deduplicated; callers stop at the first stage that returns releases.
pub fn toc_candidates_from_frames(frames: &[u32]) -> Vec<TocCandidate> {
    let n = frames.len();
    if n == 0 {
        return Vec::new();
    }
    let all: Vec<usize> = (0..n).collect();
    let is_stub = |i: usize| frames[i] < CD_MIN_TRACK_FRAMES;

    let mut stages: Vec<(&'static str, Vec<usize>)> = vec![("as-is", all.clone())];
    if is_stub(0) {
        stages.push(("dropped leading stub track", all[1..].to_vec()));
    }
    if n > 1 && is_stub(n - 1) {
        stages.push(("dropped trailing stub track", all[..n - 1].to_vec()));
    }
    if n > 2 && is_stub(0) && is_stub(n - 1) {
        stages.push(("dropped edge stub tracks", all[1..n - 1].to_vec()));
    }
    let interior: Vec<usize> = all.iter().copied().filter(|&i| !is_stub(i)).collect();
    if !interior.is_empty() {
        stages.push(("dropped all stub tracks", interior));
    }

    let mut out: Vec<TocCandidate> = Vec::new();
    for (label, kept) in stages {
        if kept.is_empty() {
            continue;
        }
        let sectors = sectors_from_kept_frames(frames, &kept);
        if out.iter().any(|c| c.sectors == sectors) {
            continue;
        }
        out.push(TocCandidate {
            sectors,
            kept_indices: kept,
            label,
        });
    }
    out
}

#[cfg(test)]
pub(crate) fn two_against_nature_frames_test_reexport() -> Vec<u32> {
    let offsets = [
        150u32, 26702, 50461, 78872, 97596, 116395, 144731, 169281, 194435, 232049,
    ];
    let leadout = 232128u32;
    let mut frames: Vec<u32> = offsets.windows(2).map(|w| w[1] - w[0]).collect();
    frames.push(leadout - offsets[offsets.len() - 1]);
    frames
}

/// Build cascade candidates from an already-synthesized `[offsets…, leadout]`
/// sector vector (the shape every existing TOC-synthesis helper produces).
/// The as-is candidate reproduces the input exactly, so cached responses for
/// historical TOC strings keep hitting.
pub fn toc_candidates_from_sectors(sectors: &[u32]) -> Vec<TocCandidate> {
    if sectors.len() < 2 {
        return Vec::new();
    }
    let frames: Vec<u32> = sectors.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
    toc_candidates_from_frames(&frames)
}

/// Align a release matched by a dropped-track candidate back onto the FULL
/// source track list: MB track `k` lands at source ordinal `kept[k]`, and
/// dropped source ordinals get neutral placeholder tracks (empty title, no
/// IDs) that the editor-population presence passes already treat as "MB is
/// silent on this track". This keeps every downstream consumer positional
/// without shifting tags onto the wrong files.
pub fn align_release_tracks_to_source(
    release: &MbRelease,
    kept: &[usize],
    total_source_tracks: usize,
) -> MbRelease {
    let mut aligned = release.clone();
    let mut tracks: Vec<MbTrack> = (0..total_source_tracks)
        .map(|i| MbTrack {
            position: (i + 1) as u32,
            track_id: None,
            recording_id: None,
            artist_id: None,
            title: String::new(),
            artist: String::new(),
            isrc: None,
            length_ms: None,
        })
        .collect();
    for (k, &src) in kept.iter().enumerate() {
        if let (Some(slot), Some(track)) = (tracks.get_mut(src), release.tracks.get(k)) {
            let mut track = track.clone();
            track.position = (src + 1) as u32;
            *slot = track;
        }
    }
    aligned.tracks = tracks;
    aligned
}

/// Outcome of a cascading TOC lookup: releases from the FIRST stage that
/// matched (already aligned back to the full source track list when that
/// stage dropped tracks), which stage matched, and the cache writes for
/// every stage that fired over the network.
#[derive(Debug)]
pub struct MbCascadeOutcome {
    pub releases: Vec<MbRelease>,
    /// The candidate that produced `releases`; `None` when no stage matched.
    pub matched: Option<TocCandidate>,
    /// 0-based source track indices excluded by the matching stage (empty
    /// for an as-is match).
    pub dropped_source_indices: Vec<usize>,
    /// `(toc_string, response_json)` for each stage that hit the network.
    pub cache_writes: Vec<(String, String)>,
}

/// Run the stub-drop cascade: try each candidate TOC in order, stopping at
/// the first stage that returns releases. `cached` must be pre-fetched
/// per-candidate (same order) so this stays database-free for
/// `tokio::spawn`. Total source track count is taken from the FIRST
/// candidate (the as-is stage always covers every source track).
pub async fn lookup_release_by_toc_cascading(
    candidates: &[TocCandidate],
    cached: Vec<Option<String>>,
) -> Result<MbCascadeOutcome, String> {
    let total = candidates
        .first()
        .map(|c| c.kept_indices.len())
        .unwrap_or(0);
    let mut cache_writes = Vec::new();
    let mut last_error: Option<String> = None;
    for (i, candidate) in candidates.iter().enumerate() {
        let cached_body = cached.get(i).cloned().flatten();
        let outcome = match lookup_release_by_toc(&candidate.sectors, cached_body).await {
            Ok(outcome) => outcome,
            Err(error) => {
                // Transport failures abort later stages (rate limiting makes
                // hammering a failing endpoint pointless) but surface the
                // error only if NO earlier stage matched.
                last_error = Some(error);
                break;
            }
        };
        if let Some(body) = outcome.cache_response {
            if let Some(toc) = build_mb_toc(&candidate.sectors) {
                cache_writes.push((toc, body));
            }
        }
        if !outcome.releases.is_empty() {
            let dropped = candidate.dropped_indices(total);
            let releases = if dropped.is_empty() {
                outcome.releases
            } else {
                outcome
                    .releases
                    .iter()
                    .map(|r| align_release_tracks_to_source(r, &candidate.kept_indices, total))
                    .collect()
            };
            return Ok(MbCascadeOutcome {
                releases,
                matched: Some(candidate.clone()),
                dropped_source_indices: dropped,
                cache_writes,
            });
        }
    }
    if let Some(error) = last_error {
        if cache_writes.is_empty() {
            return Err(error);
        }
    }
    Ok(MbCascadeOutcome {
        releases: Vec::new(),
        matched: None,
        dropped_source_indices: Vec::new(),
        cache_writes,
    })
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

    // Pass through the shared rate limiter before issuing the network
    // call. MB's 1 req/sec/IP policy applies across the whole `/ws/2/*`
    // namespace, so this gate is one global token (see `mb_acquire`).
    mb_acquire().await;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("MusicBrainz query failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
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
pub fn parse_mb_response_all(body: &str, n_tracks: usize) -> Result<Vec<MbRelease>, String> {
    parse_mb_response_all_with_sort(body, n_tracks, ReleaseSortMode::MusicBrainzScore)
}

/// Parse a text-search response with DVD-Video/DVD media preference.
///
/// Text search is the fallback for DVD-Video sources that do not match a
/// synthetic CD TOC. When MusicBrainz returns multiple plausible releases,
/// favor rows with a DVD-Video or DVD medium, and favor an exact track-count
/// match inside that subset. TOC lookups keep their native MusicBrainz score
/// ordering through `parse_mb_response_all`.
pub fn parse_mb_search_response_all(
    body: &str,
    n_tracks: usize,
) -> Result<Vec<MbRelease>, String> {
    parse_mb_response_all_with_sort(body, n_tracks, ReleaseSortMode::TextSearchPreferDvdVideo)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseSortMode {
    MusicBrainzScore,
    TextSearchPreferDvdVideo,
}

fn parse_mb_response_all_with_sort(
    body: &str,
    n_tracks: usize,
    mode: ReleaseSortMode,
) -> Result<Vec<MbRelease>, String> {
    use serde_json::Value;
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("MusicBrainz JSON parse error: {}", e))?;

    // 404 body shape: {"error": "..."} → treat as miss.
    if v.get("error").is_some() {
        return Ok(Vec::new());
    }

    let releases = match v.get("releases").and_then(|r| r.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(Vec::new()),
    };

    let mut indexed: Vec<(usize, i64, usize, &Value)> = releases
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let score = r.get("score").and_then(|s| s.as_i64()).unwrap_or(0);
            let media_rank = match mode {
                ReleaseSortMode::MusicBrainzScore => 0,
                ReleaseSortMode::TextSearchPreferDvdVideo => {
                    dvd_video_release_preference_rank(r, n_tracks)
                }
            };
            (media_rank, score, idx, r)
        })
        .collect();

    indexed.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    Ok(indexed
        .into_iter()
        .map(|(_, _, _, r)| release_from_json(r, n_tracks))
        .collect())
}

/// Convenience wrapper that returns the highest-scoring release (or
/// `None`). Used by `:cue-mb` and `:cue-fill` which auto-pick.
pub fn parse_mb_response(body: &str, n_tracks: usize) -> Result<Option<MbRelease>, String> {
    Ok(parse_mb_response_all(body, n_tracks)?.into_iter().next())
}

/// Search MusicBrainz for releases by free-form metadata (Phase B-2).
///
/// Hits `/ws/2/release/?query=…` with a Lucene query AND-joining the
/// supplied non-empty fields. When `catalog` is supplied, the first
/// attempt includes `catno:"…"`; if that returns zero results the
/// function transparently retries without the catalog clause (each
/// attempt consumes its own rate-limit token).
///
/// `year` is conventionally `"YYYY"` but hyphenated `"YYYY-MM-DD"` also
/// works — `lucene_escape` backslash-escapes the hyphens and MB's query
/// parser accepts the result.
///
/// `n_tracks` is the disc's track count; it's threaded through to
/// `release_from_json` so multi-medium `pick_medium` stays consistent
/// once B-3's detail fetch populates `media[].tracks[]`. Search-endpoint
/// responses are shallow — most callers should follow up with the
/// detail endpoint (B-3) to get per-track titles / IDs / ISRCs.
///
/// Returns `Ok(vec![])` when MB has no match. `Err(_)` is a transport
/// or parse failure the caller should surface.
pub async fn search_releases_by_query(
    artist: &str,
    album: &str,
    catalog: Option<&str>,
    year: Option<&str>,
    n_tracks: usize,
    cached: std::collections::HashMap<String, String>,
) -> Result<MbSearchOutcome, String> {
    let with_catno = build_search_query(artist, album, catalog, year);
    if with_catno.is_empty() {
        return Err("search requires at least one of artist/album/catalog/year".to_string());
    }

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut writes: Vec<(String, String)> = Vec::new();
    let first = fire_search_cached(&client, &with_catno, n_tracks, &cached, &mut writes).await?;
    if !first.is_empty() || catalog.is_none() {
        return Ok(MbSearchOutcome {
            releases: first,
            cache_writes: writes,
        });
    }

    // Catalog-first miss: retry without the catno clause. Covers
    // pressings where MB has a stripped or differently-formatted
    // catalog number. Verified 2026-05-11 against Solo Monk's SRGS
    // pressing — see project_mb_on_sacd_plan.md.
    let without_catno = build_search_query(artist, album, None, year);
    if without_catno == with_catno {
        return Ok(MbSearchOutcome {
            releases: first,
            cache_writes: writes,
        });
    }
    let second =
        fire_search_cached(&client, &without_catno, n_tracks, &cached, &mut writes).await?;
    Ok(MbSearchOutcome {
        releases: second,
        cache_writes: writes,
    })
}

/// Outcome of a Phase B-2 text/release search. `releases` is the parsed
/// shallow result list (use `fetch_release_detail` for per-track data).
/// `cache_writes` contains `(cache_key, response_json)` pairs the caller
/// should persist via `Database::store_mb_search`. May be empty when all
/// branches hit the in-memory `cached` map.
#[derive(Debug, Clone)]
pub struct MbSearchOutcome {
    pub releases: Vec<MbRelease>,
    pub cache_writes: Vec<(String, String)>,
}

/// Canonical cache key for a text/release search. Mirrors
/// `build_search_query` so two callers building the same query share
/// the cache row. Versioned (`search:v1:`) so future schema tweaks can
/// invalidate cleanly.
pub fn search_cache_key(
    artist: &str,
    album: &str,
    catalog: Option<&str>,
    year: Option<&str>,
) -> String {
    format!(
        "search:v1:{}",
        build_search_query(artist, album, catalog, year)
    )
}

async fn fire_search_cached(
    client: &reqwest::Client,
    query: &str,
    n_tracks: usize,
    cached: &std::collections::HashMap<String, String>,
    writes: &mut Vec<(String, String)>,
) -> Result<Vec<MbRelease>, String> {
    let key = format!("search:v1:{}", query);
    if let Some(body) = cached.get(&key) {
        return parse_mb_search_response_all(body, n_tracks);
    }
    let (releases, body) = fire_search(client, query, n_tracks).await?;
    writes.push((key, body));
    Ok(releases)
}

fn build_search_query(
    artist: &str,
    album: &str,
    catalog: Option<&str>,
    year: Option<&str>,
) -> String {
    let mut clauses: Vec<String> = Vec::with_capacity(4);
    if !artist.is_empty() {
        clauses.push(format!("artist:\"{}\"", lucene_escape(artist)));
    }
    if !album.is_empty() {
        clauses.push(format!("release:\"{}\"", lucene_escape(album)));
    }
    if let Some(c) = catalog.filter(|s| !s.is_empty()) {
        clauses.push(format!("catno:\"{}\"", lucene_escape(c)));
    }
    if let Some(y) = year.filter(|s| !s.is_empty()) {
        clauses.push(format!("date:{}", lucene_escape(y)));
    }
    clauses.join(" AND ")
}

async fn fire_search(
    client: &reqwest::Client,
    query: &str,
    n_tracks: usize,
) -> Result<(Vec<MbRelease>, String), String> {
    mb_acquire().await;

    let resp = client
        .get(MB_RELEASE_BASE)
        .query(&[("query", query), ("fmt", "json"), ("limit", "25")])
        .send()
        .await
        .map_err(|e| format!("MusicBrainz query failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("MusicBrainz response error: {}", e))?;

    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok((Vec::new(), body));
    }
    if !status.is_success() {
        return Err(format!("MusicBrainz returned HTTP {}", status));
    }

    let releases = parse_mb_search_response_all(&body, n_tracks)?;
    Ok((releases, body))
}

/// Fetch a single MusicBrainz release by MBID with full sub-entity
/// detail (Phase B-3). Where `search_releases_by_query` returns
/// shallow rows, this endpoint returns the per-track titles, recording
/// IDs, ISRCs, and label catalog numbers that `populate_editor_from_mb`
/// needs to fill the editor.
///
/// **Response shape differs from search/TOC.** `/ws/2/release/?query=…`
/// and `/ws/2/discid/-?toc=…` wrap matches in `{releases: [...]}`; the
/// detail endpoint returns the release object at the top level. The
/// existing `parse_mb_response_all` doesn't fit — we parse the body
/// straight through `release_from_json`.
///
/// `n_tracks` selects the right medium on multi-disc releases via
/// `release_from_json::pick_medium`. Pass the disc's track count; pass
/// `0` only when the caller genuinely doesn't know (the fallback then
/// picks `media[0]`).
///
/// Returns `Ok(None)` when MB has no release with this MBID (HTTP 404).
/// `Err(_)` is transport/parse failure.
pub async fn fetch_release_detail(
    mbid: &str,
    n_tracks: usize,
    cached_body: Option<String>,
) -> Result<MbDetailOutcome, String> {
    if mbid.is_empty() {
        return Err("fetch_release_detail requires a non-empty MBID".to_string());
    }

    if let Some(body) = cached_body {
        // Cache hit: skip the rate-limited HTTP call entirely.
        return Ok(MbDetailOutcome {
            release: Some(parse_mb_detail_response(&body, n_tracks)?),
            cache_write: None,
        });
    }

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    // `inc=` separators must reach MB as literal `+` characters; using
    // `.query()` would percent-encode them to `%2B` and break the
    // sub-entity split. Matches the TOC path's `format!` style for the
    // same reason. MBIDs from MB are UUIDs (hex + dashes, URL-safe).
    let url = format!(
        "{}{}?inc=artist-credits+isrcs+labels+recordings+release-groups&fmt=json",
        MB_RELEASE_BASE, mbid,
    );

    mb_acquire().await;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("MusicBrainz query failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("MusicBrainz response error: {}", e))?;

    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(MbDetailOutcome {
            release: None,
            cache_write: None,
        });
    }
    if !status.is_success() {
        return Err(format!("MusicBrainz returned HTTP {}", status));
    }

    let release = parse_mb_detail_response(&body, n_tracks)?;
    let key = detail_cache_key(mbid);
    Ok(MbDetailOutcome {
        release: Some(release),
        cache_write: Some((key, body)),
    })
}

/// Outcome of a Phase B-3 release-detail fetch. `release` is `None`
/// when MB returned 404 for the MBID. `cache_write` carries the
/// `(cache_key, response_json)` the caller should persist via
/// `Database::store_mb_search`; `None` when the call was served from
/// the in-memory cache or returned 404.
#[derive(Debug, Clone)]
pub struct MbDetailOutcome {
    pub release: Option<MbRelease>,
    pub cache_write: Option<(String, String)>,
}

/// Canonical cache key for a release-detail body. Shares the
/// `musicbrainz_search_cache` table with text-search bodies; the
/// `detail:v1:` prefix prevents collision with `search:v1:` rows.
pub fn detail_cache_key(mbid: &str) -> String {
    format!("detail:v1:{}", mbid)
}

/// Parse a top-level MusicBrainz release object (detail endpoint).
/// Distinct from `parse_mb_response_all` which expects the
/// `{releases: [...]}` wrapper shape returned by search and TOC.
pub fn parse_mb_detail_response(body: &str, n_tracks: usize) -> Result<MbRelease, String> {
    use serde_json::Value;
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("MusicBrainz JSON parse error: {}", e))?;

    if v.get("error").is_some() {
        return Err(format!(
            "MusicBrainz error: {}",
            v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")
        ));
    }
    if v.get("id").and_then(|i| i.as_str()).is_none() {
        return Err("MusicBrainz detail response missing top-level release id".to_string());
    }

    Ok(release_from_json(&v, n_tracks))
}


fn medium_track_count(medium: &serde_json::Value) -> u64 {
    medium
        .get("track-count")
        .and_then(|c| c.as_u64())
        .unwrap_or_else(|| {
            medium
                .get("tracks")
                .and_then(|t| t.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0)
        })
}

fn medium_format(medium: &serde_json::Value) -> Option<&str> {
    medium
        .get("format")
        .and_then(|format| format.as_str())
        .map(str::trim)
        .filter(|format| !format.is_empty())
}

fn is_dvd_video_medium_format(format: &str) -> bool {
    let normalized = format
        .trim()
        .to_ascii_lowercase()
        .replace('_', " ")
        .replace('-', " ");
    matches!(normalized.as_str(), "dvd" | "dvd video")
}

fn dvd_video_medium_rank(medium: &serde_json::Value, n_tracks: usize) -> usize {
    let Some(format) = medium_format(medium) else {
        return 0;
    };
    if !is_dvd_video_medium_format(format) {
        return 0;
    }
    if n_tracks > 0 && medium_track_count(medium) == n_tracks as u64 {
        2
    } else {
        1
    }
}

fn dvd_video_release_preference_rank(rel: &serde_json::Value, n_tracks: usize) -> usize {
    rel.get("media")
        .and_then(|v| v.as_array())
        .map(|media| {
            media
                .iter()
                .map(|medium| dvd_video_medium_rank(medium, n_tracks))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn pick_medium_for_release<'a>(
    media: &'a [serde_json::Value],
    n_tracks: usize,
) -> Option<&'a serde_json::Value> {
    if n_tracks > 0 {
        if let Some(medium) = media.iter().find(|medium| {
            medium_track_count(medium) == n_tracks as u64
                && medium_format(medium).is_some_and(is_dvd_video_medium_format)
        }) {
            return Some(medium);
        }
        if let Some(medium) = media
            .iter()
            .find(|medium| medium_track_count(medium) == n_tracks as u64)
        {
            return Some(medium);
        }
    }
    media.first()
}

fn release_from_json(rel: &serde_json::Value, n_tracks: usize) -> MbRelease {
    let release_id = rel
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = rel
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
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
    // medium whose track count matches the queried TOC/search track count.
    // If more than one medium has that count, prefer DVD-Video/DVD so
    // DVD-Video text-search results populate the authored video track list
    // rather than a sibling CD medium with the same number of tracks.
    let media = rel
        .get("media")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let pick_medium = pick_medium_for_release(media, n_tracks);
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
        disc_count: media.len(),
        tracks,
    }
}

fn track_from_json(t: &serde_json::Value) -> Option<MbTrack> {
    let position = t.get("position").and_then(|v| v.as_u64())? as u32;
    let title = t
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let track_artist = artist_credit_string(t.get("artist-credit"));
    let artist = if track_artist.is_empty() {
        artist_credit_string(t.get("recording").and_then(|r| r.get("artist-credit")))
    } else {
        track_artist
    };
    let track_id = t
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let recording_id = t
        .get("recording")
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let artist_id = t
        .get("artist-credit")
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
    let length_ms = t
        .get("length")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            t.get("recording")
                .and_then(|r| r.get("length"))
                .and_then(|v| v.as_u64())
        })
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
/// Called from `populate_editor_from_mb` to supplement the
/// user-reviewed Title/Artist/Album/Year/Genre with the rest of MB's
/// data. Empty values are skipped (preserves whatever the file had).
pub fn populate_editor_mb_supplemental(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) {
    let decision = compute_per_track_decision_blocking(&state.active_surface().paths, release);
    populate_editor_mb_supplemental_with_per_track_decision(state, release, &decision);
}

/// Populate MusicBrainz-only metadata fields using a precomputed per-track
/// decision. This variant performs no media/tag probing and is safe for the
/// event-loop thread.
pub fn populate_editor_mb_supplemental_with_per_track_decision(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
    decision: &PerTrackDecision,
) {
    use lofty::tag::ItemKey;

    let n = state.active_surface().paths.len();
    // Single-image rip: one file representing a multi-track release.
    let single_image = n == 1 && release.tracks.len() > 1;
    // Per-track populate eligibility: same guards used to gate the
    // populate-time CUESHEET embed pre-Phase-5. When false on a
    // single_image rip we fall back to album-only writes for the
    // legacy MB-only IDs that have no per-track home.
    let per_track_populate = single_image && decision.per_track_populate;

    fn find_or_create(
        entries: &mut Vec<crate::tui::probe::TagEntry>,
        key: &str,
        item_key: ItemKey,
        dim: usize,
    ) -> usize {
        if let Some(i) = entries
            .iter()
            .position(|e| e.display_key.eq_ignore_ascii_case(key))
        {
            return i;
        }
        entries.push(crate::tui::probe::TagEntry {
            display_key: key.to_string(),
            item_key,
            value: String::new(),
            original: String::new(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec![String::new(); dim],
            per_file_originals: vec![String::new(); dim],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
        entries.len() - 1
    }

    // Per-track presence pre-pass: only create the entry when at least
    // one track in the release has data for that field. Tracks with no
    // data leave their per-track value as the empty string (matching
    // the pre-existing "MB silent on this track" behavior).
    //
    // ISRC is the one per-track ID that has a home on a single-image
    // rip — the embedded CUESHEET stores per-track ISRC. It populates
    // per-track only when `per_track_populate`; on a single_image rip
    // with failed guards (multi-disc / sidecar / unverifiable identity)
    // it's skipped entirely (no per-track CUESHEET to land in, and
    // album-level ISRC has no meaning).
    //
    // The other per-track MB IDs (MUSICBRAINZ_RECORDINGID /
    // RELEASETRACKID / ARTISTID) have no CUESHEET field and a file's
    // tag system holds only one of each, so they stay album-only and
    // are gated on `!single_image`.
    let any_isrc = (per_track_populate || !single_image)
        && release
            .tracks
            .iter()
            .any(|t| t.isrc.as_deref().is_some_and(|s| !s.is_empty()));
    let any_recording = !single_image
        && release
            .tracks
            .iter()
            .any(|t| t.recording_id.as_deref().is_some_and(|s| !s.is_empty()));
    let any_track = !single_image
        && release
            .tracks
            .iter()
            .any(|t| t.track_id.as_deref().is_some_and(|s| !s.is_empty()));
    let any_track_artist = !single_image
        && release
            .tracks
            .iter()
            .any(|t| t.artist_id.as_deref().is_some_and(|s| !s.is_empty()));

    // ISRC dim: per-track when `per_track_populate`, per-file otherwise.
    let isrc_dim = if per_track_populate {
        release.tracks.len()
    } else {
        n
    };
    let isrc_idx = if any_isrc {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "ISRC",
            ItemKey::Isrc,
            isrc_dim,
        ))
    } else {
        None
    };
    // Pre-existing ISRC entry on a per-track-eligible single-image rip
    // (Phase 2 may have surfaced per-track ISRCs from the embedded
    // CUESHEET): grow / shrink to MB's track count, replicating the
    // first existing slot into padded positions so revert keeps the
    // pre-populate state.
    if per_track_populate {
        if let Some(idx) = isrc_idx {
            crate::tui::probe::ensure_dim_replicate(&mut state.active_surface_mut().entries[idx], isrc_dim);
        }
    }
    let recording_idx = if any_recording {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_TRACKID",
            ItemKey::MusicBrainzRecordingId,
            n,
        ))
    } else {
        None
    };
    let track_idx = if any_track {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_RELEASETRACKID",
            ItemKey::MusicBrainzTrackId,
            n,
        ))
    } else {
        None
    };
    let artist_idx = if any_track_artist {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_ARTISTID",
            ItemKey::MusicBrainzArtistId,
            n,
        ))
    } else {
        None
    };

    // Album-level — gate each entry on MB actually having a value.
    let catalog_value = release
        .catalog
        .as_deref()
        .or(release.barcode.as_deref())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let catalog_idx = if catalog_value.is_some() {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "CATALOGNUMBER",
            ItemKey::CatalogNumber,
            n,
        ))
    } else {
        None
    };
    let album_id_idx = if !release.release_id.is_empty() {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_ALBUMID",
            ItemKey::MusicBrainzReleaseId,
            n,
        ))
    } else {
        None
    };
    let album_artist_id_idx = if release.artist_id.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_ALBUMARTISTID",
            ItemKey::MusicBrainzReleaseArtistId,
            n,
        ))
    } else {
        None
    };
    let release_group_idx = if release
        .release_group_id
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "MUSICBRAINZ_RELEASEGROUPID",
            ItemKey::MusicBrainzReleaseGroupId,
            n,
        ))
    } else {
        None
    };
    let original_date_idx = if release
        .original_date
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "ORIGINALDATE",
            ItemKey::OriginalReleaseDate,
            n,
        ))
    } else {
        None
    };
    let country_idx = if release.country.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "RELEASECOUNTRY",
            ItemKey::Unknown("RELEASECOUNTRY".to_string()),
            n,
        ))
    } else {
        None
    };

    // Per-track ISRC writes for per_track_populate: the CUESHEET-
    // friendly dim != paths.len() case, so the per-file loop below
    // can't address tracks 1..N. Done as a dedicated pass over MB
    // tracks.
    if per_track_populate {
        if let Some(idx) = isrc_idx {
            for mt in release.tracks.iter() {
                let i = (mt.position as usize).saturating_sub(1);
                if i >= state.active_surface().entries[idx].per_file_values.len() {
                    continue;
                }
                if let Some(s) = mt.isrc.as_deref().filter(|s| !s.is_empty()) {
                    state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
                }
            }
        }
    }

    for i in 0..n {
        if let Some(mt) = release.tracks.iter().find(|m| m.position as usize == i + 1) {
            if !per_track_populate {
                if let (Some(idx), Some(s)) =
                    (isrc_idx, mt.isrc.as_deref().filter(|s| !s.is_empty()))
                {
                    state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
                }
            }
            if let (Some(idx), Some(s)) = (
                recording_idx,
                mt.recording_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
            }
            if let (Some(idx), Some(s)) =
                (track_idx, mt.track_id.as_deref().filter(|s| !s.is_empty()))
            {
                state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
            }
            if let (Some(idx), Some(s)) = (
                artist_idx,
                mt.artist_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
            }
        }
        if let (Some(idx), Some(s)) = (catalog_idx, catalog_value.as_deref()) {
            state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
        }
        if let Some(idx) = album_id_idx {
            state.active_surface_mut().entries[idx].per_file_values[i] = release.release_id.clone();
        }
        if let (Some(idx), Some(s)) = (
            album_artist_id_idx,
            release.artist_id.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            release_group_idx,
            release
                .release_group_id
                .as_deref()
                .filter(|s| !s.is_empty()),
        ) {
            state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            original_date_idx,
            release.original_date.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
        }
        if let (Some(idx), Some(s)) = (
            country_idx,
            release.country.as_deref().filter(|s| !s.is_empty()),
        ) {
            state.active_surface_mut().entries[idx].per_file_values[i] = s.to_string();
        }
    }

    for idx in [
        isrc_idx,
        recording_idx,
        track_idx,
        artist_idx,
        catalog_idx,
        album_id_idx,
        album_artist_id_idx,
        release_group_idx,
        original_date_idx,
        country_idx,
    ]
    .iter()
    .filter_map(|x| *x)
    {
        recompute_and_stamp_mb_proposed(&mut state.active_surface_mut().entries[idx], n);
    }

    crate::tui::probe::sort_entries_standard_first_existing_only(&mut state.active_surface_mut().entries);
    state.active_surface_mut().dirty = true;
}

/// Recompute `value` / `is_mixed` for an entry after a populate touched
/// its `per_file_values`, and stamp `mb_proposed_value` /
/// `mb_proposed_per_file` so the editor can show a `[revert]` /
/// `[use MB]` toggle pill.
///
/// Stamps only when the resulting value actually differs from the
/// pre-populate `original` — fields where MB happened to match what the
/// file already had don't need a toggle.
/// Single-image-rip eligibility for per-track MB populate. Returns
/// false when any guard fails:
/// - multi-disc release (per-track across discs has no embedded-CUE
///   home — one file can only hold one CUESHEET tag)
/// - sidecar `.cue` file present alongside the audio (the sidecar is
///   the canonical per-track truth; we don't fight it)
/// - file identity / duration unverifiable against the release
///   (matching MUSICBRAINZ_ALBUMID tag OR probe-verified duration
///   within ±3s; missing tag + failed probe + duration mismatch all
///   skip)
///
/// Returns `Some(reason)` describing why per-track populate would be
/// skipped, or `None` when eligibility checks pass. Callers branch on
/// `is_none()` for the boolean decision; the public-facing event-loop
/// caller also surfaces the reason in `app.set_status` so users
/// understand why no CUESHEET row appeared.
///
/// Returns None on the not-applicable case (multi-file or single-track
/// Phase C item 3: format a track-count divergence warning for the
/// status line when populate's editor row count won't equal the MB
/// release's track count. Non-fatal — populate writes what it can
/// match by position; the message exists to tell the user why some
/// tracks didn't get tagged.
///
/// **Single-image guard:** a 1-file editor with N>1 MB tracks is NOT
/// a mismatch — per-track titles ride in the embedded CUESHEET tag
/// rather than in N separate files. The helper returns `None` for
/// that shape. Mismatches with multi-file editors and SACD areas
/// (where `paths.len()` reflects the area's track count) fire
/// normally.
///
/// Inner helper `count_mismatch_text` takes numbers directly so the
/// branching is unit-testable without building a full
/// `MetadataEditorState`.
pub fn track_count_mismatch_message(
    state: &crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) -> Option<String> {
    let n_files = state.active_surface().paths.len();
    let n_mb = release.tracks.len();
    if n_files == 1 && n_mb > 1 {
        return None;
    }
    count_mismatch_text(n_files, n_mb)
}

pub(super) fn count_mismatch_text(n_files: usize, n_mb: usize) -> Option<String> {
    if n_files == n_mb {
        return None;
    }
    Some(format!(
        "MB release has {} track{}, editor has {}",
        n_mb,
        if n_mb == 1 { "" } else { "s" },
        n_files,
    ))
}

const DVDV_TRACK_DURATION_WARNING_TOLERANCE_MS: u64 = 5_000;
const DVDV_DURATION_WARNING_KEY: &str = "DVDV_DURATION_WARNING";

#[derive(Debug, Clone, PartialEq, Eq)]
struct DvdvTrackDurationMismatch {
    track_number: usize,
    dvd_ms: u64,
    mb_ms: u64,
    diff_ms: u64,
}

/// Compare the active DVD-Video editor presentation against the selected
/// MusicBrainz release after positional assignment. Mismatches beyond five
/// seconds are non-fatal: the editor keeps the MB tags, shows a synthetic
/// warning row, and returns a status-line summary.
pub fn apply_dvdv_duration_warnings(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) -> Option<String> {
    let Some(durations) = state.active_surface().dvdv_track_durations.as_deref() else {
        return None;
    };
    if durations.is_empty() || state.active_surface().paths.len() != durations.len() {
        return None;
    }

    let mismatches = dvdv_duration_mismatches(
        durations,
        release,
        DVDV_TRACK_DURATION_WARNING_TOLERANCE_MS,
    );
    upsert_dvdv_duration_warning_entry(state, &mismatches);
    dvdv_duration_warning_summary(&mismatches, DVDV_TRACK_DURATION_WARNING_TOLERANCE_MS)
}

fn dvdv_duration_mismatches(
    durations: &[f64],
    release: &MbRelease,
    tolerance_ms: u64,
) -> Vec<DvdvTrackDurationMismatch> {
    let mut mismatches = Vec::new();
    for (idx, duration_secs) in durations.iter().enumerate() {
        if !(duration_secs.is_finite() && *duration_secs > 0.0) {
            continue;
        }
        let track_number = idx + 1;
        let Some(mb_ms) = release
            .tracks
            .iter()
            .find(|track| track.position as usize == track_number)
            .and_then(|track| track.length_ms)
        else {
            continue;
        };
        let dvd_ms = (*duration_secs * 1000.0).round().max(0.0) as u64;
        let mb_ms = u64::from(mb_ms);
        let diff_ms = dvd_ms.abs_diff(mb_ms);
        if diff_ms > tolerance_ms {
            mismatches.push(DvdvTrackDurationMismatch {
                track_number,
                dvd_ms,
                mb_ms,
                diff_ms,
            });
        }
    }
    mismatches
}

fn dvdv_duration_warning_summary(
    mismatches: &[DvdvTrackDurationMismatch],
    tolerance_ms: u64,
) -> Option<String> {
    let first = mismatches.first()?;
    Some(format!(
        "DVD-Video duration warning: {} track{} differ by >{}s; first is track {} (DVD {}, MB {}, diff {})",
        mismatches.len(),
        if mismatches.len() == 1 { "" } else { "s" },
        tolerance_ms / 1000,
        first.track_number,
        format_duration_ms(first.dvd_ms),
        format_duration_ms(first.mb_ms),
        format_duration_ms(first.diff_ms),
    ))
}

fn upsert_dvdv_duration_warning_entry(
    state: &mut crate::tui::app::MetadataEditorState,
    mismatches: &[DvdvTrackDurationMismatch],
) {
    let n = state.active_surface().paths.len();
    if n == 0 {
        return;
    }
    let mut per_file_values = vec![String::new(); n];
    for mismatch in mismatches {
        if let Some(slot) = per_file_values.get_mut(mismatch.track_number.saturating_sub(1)) {
            *slot = format!(
                "DVD {} vs MB {} (diff {})",
                format_duration_ms(mismatch.dvd_ms),
                format_duration_ms(mismatch.mb_ms),
                format_duration_ms(mismatch.diff_ms),
            );
        }
    }
    let summary = dvdv_duration_warning_summary(
        mismatches,
        DVDV_TRACK_DURATION_WARNING_TOLERANCE_MS,
    )
    .unwrap_or_default();
    let all_same = per_file_values.windows(2).all(|window| window[0] == window[1]);

    if let Some(idx) = state.active_surface()
        .entries
        .iter()
        .position(|entry| entry.display_key.eq_ignore_ascii_case(DVDV_DURATION_WARNING_KEY))
    {
        let entry = &mut state.active_surface_mut().entries[idx];
        entry.value = summary.clone();
        entry.original.clear();
        entry.is_binary = false;
        entry.is_mixed = !all_same && n > 1;
        entry.per_file_values = per_file_values;
        entry.per_file_originals = vec![String::new(); n];
        entry.mb_proposed_value = None;
        entry.mb_proposed_per_file = None;
    } else if !mismatches.is_empty() {
        state.active_surface_mut().entries.push(crate::tui::probe::TagEntry {
            display_key: DVDV_DURATION_WARNING_KEY.to_string(),
            item_key: lofty::tag::ItemKey::Unknown(DVDV_DURATION_WARNING_KEY.to_string()),
            value: summary,
            original: String::new(),
            is_binary: false,
            is_mixed: !all_same && n > 1,
            per_file_values,
            per_file_originals: vec![String::new(); n],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }
}

fn format_duration_ms(ms: u64) -> String {
    let total_secs = (ms + 500) / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{}:{:02}", minutes, seconds)
}

/// Result of the single-image MusicBrainz per-track gate.
///
/// Computing this can read the file's tags with `lofty` and probe the audio
/// sample count. Event-loop reducers must therefore compute it on a blocking
/// worker and pass the value into `populate_editor_from_mb_with_per_track_decision`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerTrackDecision {
    pub per_track_populate: bool,
    pub skip_reason: Option<String>,
}

/// Blocking guard used by `:tags-mb` before per-track single-image population.
///
/// This function may call `lofty::read_from_path()` and
/// `accuraterip::probe_sample_count()`. Call it only from tests, CLI-style
/// synchronous code, or a `spawn_blocking` worker. TUI message handlers should
/// use the precomputed `PerTrackDecision` carried by `AppMessage::TagsMbApplyReady`.
pub fn compute_per_track_decision_blocking(
    paths: &[std::path::PathBuf],
    release: &MbRelease,
) -> PerTrackDecision {
    if paths.len() != 1 || release.tracks.len() <= 1 {
        return PerTrackDecision::default();
    }
    if release.disc_count > 1 {
        return PerTrackDecision {
            per_track_populate: false,
            skip_reason: Some(format!(
                "multi-disc release ({} discs) — album-level tags only",
                release.disc_count,
            )),
        };
    }
    // Note: a sidecar .cue is no longer a guard. The editor's
    // open-time inject_sidecar_cuesheet_if_present surfaces the
    // sidecar's per-track structure as a synthetic embedded CUESHEET
    // entry, so :tags-mb can populate per-track on top and Phase 4
    // can persist edits as an embedded CUESHEET tag. The sidecar on
    // disk is left untouched.
    if let Some(reason) = paths
        .first()
        .and_then(|p| verify_single_image_matches_release(p, release))
    {
        return PerTrackDecision {
            per_track_populate: false,
            skip_reason: Some(format!("{} — album-level tags only", reason)),
        };
    }
    // Even with identity (MUSICBRAINZ_ALBUMID match) or duration
    // verification passing, we need MB track lengths to generate a
    // meaningful CUESHEET — Phase 5's `cue_from_mb_release` fails
    // without them, leaving per-track entries with no anchor and
    // forcing Phase 4 to refuse saves. Tighten here so the user
    // gets the album-level fallback in this corner instead.
    if release.tracks.iter().any(|t| t.length_ms.is_none()) {
        return PerTrackDecision {
            per_track_populate: false,
            skip_reason: Some("MB release missing track lengths — album-level tags only".to_string()),
        };
    }
    PerTrackDecision {
        per_track_populate: true,
        skip_reason: None,
    }
}

/// Blocking compatibility wrapper around `compute_per_track_decision_blocking`.
/// Event-loop reducers must not call this directly.
#[cfg(test)]
pub(super) fn per_track_skip_reason(
    paths: &[std::path::PathBuf],
    release: &MbRelease,
) -> Option<String> {
    compute_per_track_decision_blocking(paths, release).skip_reason
}

/// Boolean wrapper around the blocking per-track gate. Kept for synchronous
/// callers and tests; event-loop reducers pass a precomputed decision instead.
#[cfg(test)]
fn is_per_track_eligible(paths: &[std::path::PathBuf], release: &MbRelease, verbose: bool) -> bool {
    let decision = compute_per_track_decision_blocking(paths, release);
    if let Some(reason) = decision.skip_reason.as_deref() {
        if verbose {
            log::info!(":tags-mb: per-track populate skipped ({})", reason);
        }
    }
    decision.per_track_populate
}

// `ensure_dim_replicate` moved to probe.rs so gnudb can share it.

fn recompute_and_stamp_mb_proposed(entry: &mut crate::tui::probe::TagEntry, _n: usize) {
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    let dim = entry.per_file_values.len();
    entry.is_mixed = !all_same && dim > 1;
    entry.value = if entry.is_mixed {
        "<multiple values>".to_string()
    } else {
        entry.per_file_values.first().cloned().unwrap_or_default()
    };

    if entry.value != entry.original || entry.per_file_values != entry.per_file_originals {
        entry.mb_proposed_value = Some(entry.value.clone());
        entry.mb_proposed_per_file = Some(entry.per_file_values.clone());
    }
}

/// Blocking compatibility wrapper for MusicBrainz editor population.
///
/// This computes the single-image guard in place, which may touch media files.
/// TUI event-loop reducers must use
/// `populate_editor_from_mb_with_per_track_decision` with a worker-computed
/// `PerTrackDecision` instead.
pub fn populate_editor_from_mb(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
) {
    let decision = compute_per_track_decision_blocking(&state.active_surface().paths, release);
    populate_editor_from_mb_with_per_track_decision(state, release, &decision);
}

/// Populate a metadata editor state with values from a MusicBrainz release
/// using a precomputed single-image per-track decision.
///
/// Track-level fields (TITLE, ARTIST, TRACKNUMBER, ISRC) come from the
/// matching `MbTrack.position`; album-level fields (ALBUM, DATE,
/// CATALOGNUMBER) apply to every track. Existing per-file tag values are only
/// overwritten when the MB value is non-empty. This reducer does not perform
/// media/tag probing, so it is safe to call from the event-loop thread as long
/// as `decision` was computed on a blocking worker.
pub fn populate_editor_from_mb_with_per_track_decision(
    state: &mut crate::tui::app::MetadataEditorState,
    release: &MbRelease,
    decision: &PerTrackDecision,
) {
    use lofty::tag::ItemKey;

    populate_editor_mb_supplemental_with_per_track_decision(state, release, decision);

    let n = state.active_surface().paths.len();

    fn find_or_create(
        entries: &mut Vec<crate::tui::probe::TagEntry>,
        key: &str,
        item_key: ItemKey,
        dim: usize,
    ) -> usize {
        if let Some(i) = entries
            .iter()
            .position(|e| e.display_key.eq_ignore_ascii_case(key))
        {
            return i;
        }
        entries.push(crate::tui::probe::TagEntry {
            display_key: key.to_string(),
            item_key,
            value: String::new(),
            original: String::new(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec![String::new(); dim],
            per_file_originals: vec![String::new(); dim],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
        entries.len() - 1
    }

    // Single-image rip detection: one file, multi-track release.
    // Per-track populate fires only when guards pass (handled by
    // is_per_track_eligible — multi-disc / sidecar .cue / unverifiable
    // identity all fall back to album-level).
    let single_image = n == 1 && release.tracks.len() > 1;
    let per_track_populate = single_image && decision.per_track_populate;
    if single_image && !per_track_populate {
        if let Some(reason) = decision.skip_reason.as_deref() {
            log::info!(":tags-mb: per-track populate skipped ({})", reason);
        }
    }
    let track_dim = if per_track_populate {
        release.tracks.len()
    } else {
        n
    };

    // Per-track presence pre-pass: only create entries when at least
    // one track in the release has data for that field.
    let any_title = release.tracks.iter().any(|t| !t.title.is_empty());
    let any_artist = release.tracks.iter().any(|t| !t.artist.is_empty());

    let title_idx = if any_title {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "TITLE",
            ItemKey::TrackTitle,
            track_dim,
        ))
    } else {
        None
    };
    let artist_idx = if any_artist {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "ARTIST",
            ItemKey::TrackArtist,
            track_dim,
        ))
    } else {
        None
    };
    let album_idx = if !release.title.is_empty() {
        Some(find_or_create(
            &mut state.active_surface_mut().entries,
            "ALBUM",
            ItemKey::AlbumTitle,
            n,
        ))
    } else {
        None
    };
    // TRACKNUMBER is always 1-based-by-file-position, computed locally —
    // doesn't depend on MB content. Always create.
    let tn_idx = find_or_create(&mut state.active_surface_mut().entries, "TRACKNUMBER", ItemKey::TrackNumber, n);
    let date_idx = if release.year.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(find_or_create(&mut state.active_surface_mut().entries, "DATE", ItemKey::Year, n))
    } else {
        None
    };

    // For pre-existing TITLE/ARTIST entries on a per-track-eligible
    // rip (Phase 2 may have parsed them from an embedded CUESHEET, or
    // the file may have a single TITLE tag carrying the album name),
    // grow or shrink the entry to MB's track count. The first
    // existing value is replicated to padded slots so revert restores
    // the pre-populate state cleanly. :tags-mb is an explicit user
    // request to overwrite from MB; track-count divergence between
    // Phase 2's CUESHEET dim and MB's track count is resolved in MB's
    // favor.
    if per_track_populate {
        if let Some(idx) = title_idx {
            crate::tui::probe::ensure_dim_replicate(&mut state.active_surface_mut().entries[idx], track_dim);
        }
        if let Some(idx) = artist_idx {
            crate::tui::probe::ensure_dim_replicate(&mut state.active_surface_mut().entries[idx], track_dim);
        }
    }

    if per_track_populate {
        // Per-track populate: TITLE / ARTIST flow into per_file_values
        // by track position. Album-level fields (ALBUM, DATE) and
        // TRACKNUMBER stay at file dimension (one element).
        for i in 0..track_dim {
            let track_pos = (i + 1) as u32;
            let mt = release.tracks.iter().find(|m| m.position == track_pos);
            if let Some(mt) = mt {
                if let (Some(idx), false) = (title_idx, mt.title.is_empty()) {
                    state.active_surface_mut().entries[idx].per_file_values[i] = mt.title.clone();
                }
                if let (Some(idx), false) = (artist_idx, mt.artist.is_empty()) {
                    state.active_surface_mut().entries[idx].per_file_values[i] = mt.artist.clone();
                }
            }
        }
        if let Some(idx) = album_idx {
            state.active_surface_mut().entries[idx].per_file_values[0] = release.title.clone();
        }
        state.active_surface_mut().entries[tn_idx].per_file_values[0] = "1".to_string();
        if let (Some(idx), Some(year)) =
            (date_idx, release.year.as_deref().filter(|s| !s.is_empty()))
        {
            state.active_surface_mut().entries[idx].per_file_values[0] = year.to_string();
        }
    } else if single_image {
        // Single-image rip with a guard failure (multi-disc release,
        // sidecar `.cue` present, or unverifiable identity / duration).
        // Album-level fallback: write the album title / artist to
        // dim-1 entries; SKIP entries already grown to per-track dim
        // by Phase 2 (e.g. file has both an embedded CUESHEET and a
        // sidecar — Phase 2 still parses the embedded one on open).
        // Writing the album title to slot [0] of a per-track TITLE
        // entry would contaminate track 0 with the album name.
        let title_dim_one = title_idx.filter(|&i| state.active_surface().entries[i].per_file_values.len() == 1);
        let artist_dim_one = artist_idx.filter(|&i| state.active_surface().entries[i].per_file_values.len() == 1);
        if let Some(idx) = title_dim_one {
            state.active_surface_mut().entries[idx].per_file_values[0] = release.title.clone();
        }
        if let (Some(idx), false) = (artist_dim_one, release.artist.is_empty()) {
            state.active_surface_mut().entries[idx].per_file_values[0] = release.artist.clone();
        }
        if let Some(idx) = album_idx {
            state.active_surface_mut().entries[idx].per_file_values[0] = release.title.clone();
        }
        state.active_surface_mut().entries[tn_idx].per_file_values[0] = "1".to_string();
        if let (Some(idx), Some(year)) =
            (date_idx, release.year.as_deref().filter(|s| !s.is_empty()))
        {
            state.active_surface_mut().entries[idx].per_file_values[0] = year.to_string();
        }
    } else {
        // Per-file populate: tag-per-file with track position == file
        // index + 1.
        for i in 0..n {
            let mt = release.tracks.iter().find(|m| m.position as usize == i + 1);
            if let Some(mt) = mt {
                if let (Some(idx), false) = (title_idx, mt.title.is_empty()) {
                    state.active_surface_mut().entries[idx].per_file_values[i] = mt.title.clone();
                }
                if let (Some(idx), false) = (artist_idx, mt.artist.is_empty()) {
                    state.active_surface_mut().entries[idx].per_file_values[i] = mt.artist.clone();
                }
            }
            if let Some(idx) = album_idx {
                state.active_surface_mut().entries[idx].per_file_values[i] = release.title.clone();
            }
            state.active_surface_mut().entries[tn_idx].per_file_values[i] = (i + 1).to_string();
            if let (Some(idx), Some(year)) =
                (date_idx, release.year.as_deref().filter(|s| !s.is_empty()))
            {
                state.active_surface_mut().entries[idx].per_file_values[i] = year.to_string();
            }
        }
    }

    for idx in [title_idx, artist_idx, album_idx, Some(tn_idx), date_idx]
        .iter()
        .filter_map(|x| *x)
    {
        recompute_and_stamp_mb_proposed(&mut state.active_surface_mut().entries[idx], n);
    }

    // Per-track-eligible single-image rip without an existing CUESHEET
    // tag: stamp one from MB so Phase 4 has structural anchors (FILE
    // line, INDEX timestamps) to mutate at save time. When the file
    // already has an embedded CUESHEET (Phase 2 parsed it on open),
    // leave it alone — Phase 4 will mutate that one in place using
    // user edits + β album-level re-derive.
    //
    // Guard checks (multi-disc / sidecar / identity) live in
    // is_per_track_eligible, not duplicated here.
    if per_track_populate {
        let has_cuesheet = state.active_surface()
            .entries
            .iter()
            .any(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"));
        if !has_cuesheet {
            if let Some(filename) = state.active_surface()
                .paths
                .first()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
            {
                let ext = std::path::Path::new(filename)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("flac");
                if let Ok(cue) = super::cue_generate::cue_from_mb_release(release, filename, ext) {
                    let cue_idx = find_or_create(
                        &mut state.active_surface_mut().entries,
                        "CUESHEET",
                        ItemKey::Unknown("CUESHEET".to_string()),
                        n,
                    );
                    state.active_surface_mut().entries[cue_idx].per_file_values[0] = cue.clone();
                    // is_binary keeps inline edit blocked; the value
                    // would be 1-2KB of multi-line content otherwise.
                    state.active_surface_mut().entries[cue_idx].is_binary = true;
                    recompute_and_stamp_mb_proposed(&mut state.active_surface_mut().entries[cue_idx], n);
                }
            }
        }
    }

    crate::tui::probe::sort_entries_standard_first(&mut state.active_surface_mut().entries);
    state.active_surface_mut().dirty = true;
}

/// Tolerance (ms) for the file-duration vs MB-track-sum check used by
/// the single-image CUESHEET-embed gate. Generous enough to absorb
/// frame-rounding (75 frames/sec ≈ 13ms × N tracks), short pregaps,
/// and small encoder padding; tight enough to catch the "user has 1
/// track of 10" case (off by tens of minutes).
const DURATION_MISMATCH_TOLERANCE_MS: u64 = 3000;

/// Pure predicate: are the two durations within `tolerance_ms` of each
/// other? Subtraction is order-safe (works whether file is shorter or
/// longer than the release total).
fn durations_consistent(file_ms: u64, release_total_ms: u64, tolerance_ms: u64) -> bool {
    let diff = if file_ms > release_total_ms {
        file_ms - release_total_ms
    } else {
        release_total_ms - file_ms
    };
    diff <= tolerance_ms
}

/// True when the file already carries a MUSICBRAINZ_ALBUMID tag whose
/// value matches `release.release_id`. Strong identity signal: the
/// file was previously tagged as belonging to this MB release, so we
/// can embed a CUESHEET confidently even if duration verification
/// can't run (corrupt header, locked file, exotic codec).
///
/// Returns false when:
/// - `release.release_id` is empty (no anchor to verify against)
/// - lofty can't read the file or its primary tag
/// - the MUSICBRAINZ_ALBUMID tag is absent or doesn't match
fn release_already_tagged_on_file(audio_path: &std::path::Path, release: &MbRelease) -> bool {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    if release.release_id.is_empty() {
        return false;
    }
    if super::probe::recover_flac_metadata_before_read(audio_path).is_err() {
        return false;
    }
    let Ok(tagged) = lofty::read_from_path(audio_path) else {
        return false;
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return false;
    };
    matches!(
        tag.get_string(&ItemKey::MusicBrainzReleaseId),
        Some(s) if s == release.release_id,
    )
}

/// Tri-state result of comparing the audio file's actual duration to
/// the sum of MB track lengths. The caller decides what to do with
/// each variant; in particular, `Unverifiable` is treated as a strict
/// skip when no other identity signal (e.g. matching MB album-id tag)
/// vouches for the file.
enum DurationCheck {
    /// Probe succeeded; durations are within
    /// `DURATION_MISMATCH_TOLERANCE_MS`.
    Match,
    /// Probe succeeded; durations diverge beyond tolerance. Carries
    /// `(file_ms, release_total_ms)` for logging.
    Mismatch(u64, u64),
    /// Couldn't determine match/mismatch: any of probe failure
    /// (corrupt header, locked file, exotic codec, missing file),
    /// zero probed duration, or any track missing `length_ms`.
    Unverifiable,
}

fn check_file_duration(audio_path: &std::path::Path, release: &MbRelease) -> DurationCheck {
    let Some(release_total_ms) = release
        .tracks
        .iter()
        .map(|t| t.length_ms.map(|n| n as u64))
        .sum::<Option<u64>>()
    else {
        return DurationCheck::Unverifiable;
    };
    let (samples, sample_rate) = match super::accuraterip::probe_sample_count(audio_path) {
        Ok(values) => values,
        Err(_) => return DurationCheck::Unverifiable,
    };
    if samples == 0 || sample_rate == 0 {
        return DurationCheck::Unverifiable;
    }
    let file_ms = ((samples as f64 / sample_rate as f64) * 1000.0).round() as u64;
    if durations_consistent(file_ms, release_total_ms, DURATION_MISMATCH_TOLERANCE_MS) {
        DurationCheck::Match
    } else {
        DurationCheck::Mismatch(file_ms, release_total_ms)
    }
}

/// Decide whether to embed a CUESHEET tag into the file.
///
/// Returns `None` when verified to match the release (proceed with
/// embed); `Some(reason)` when the file should NOT be embedded (caller
/// logs the reason and skips).
///
/// Decision flow:
/// 1. If the file already carries `MUSICBRAINZ_ALBUMID == release.release_id`
///    we treat it as definitively this release — embed even if probe
///    can't run (corrupt/locked/exotic).
/// 2. Otherwise, require a positive duration verification:
///    - `Match` → proceed
///    - `Mismatch` → skip with diagnostic
///    - `Unverifiable` → skip strictly (no positive evidence)
fn verify_single_image_matches_release(
    audio_path: &std::path::Path,
    release: &MbRelease,
) -> Option<String> {
    if release_already_tagged_on_file(audio_path, release) {
        return None;
    }
    match check_file_duration(audio_path, release) {
        DurationCheck::Match => None,
        DurationCheck::Mismatch(file_ms, total_ms) => Some(format!(
            "duration mismatch: file {}ms vs MB total {}ms (>{}ms tolerance)",
            file_ms, total_ms, DURATION_MISMATCH_TOLERANCE_MS,
        )),
        DurationCheck::Unverifiable => Some(
            "can't verify file matches release (probe failed or no MUSICBRAINZ_ALBUMID tag)"
                .to_string(),
        ),
    }
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
    // ── stub-drop TOC cascade ──────────────────────────────────────────

    // Real-world fixture lives in two_against_nature_frames_test_reexport():
    // Two Against Nature DVD-A stereo group — nine CD-accurate tracks plus a
    // 79-frame (≈1.05 s) spurious stub, reconstructed from the cached failing
    // TOC.

    #[test]
    fn cascade_as_is_candidate_matches_historical_toc() {
        let frames = super::two_against_nature_frames_test_reexport();
        let candidates = super::toc_candidates_from_frames(&frames);
        assert_eq!(candidates[0].label, "as-is");
        assert_eq!(
            super::build_mb_toc(&candidates[0].sectors).unwrap(),
            "1+10+232128+150+26702+50461+78872+97596+116395+144731+169281+194435+232049",
            "as-is candidate must be byte-identical to the historical TOC so cache entries keep hitting"
        );
        assert_eq!(candidates[0].kept_indices, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn cascade_drops_trailing_stub_and_produces_cd_accurate_nine_track_toc() {
        let frames = super::two_against_nature_frames_test_reexport();
        let candidates = super::toc_candidates_from_frames(&frames);
        let dropped_last = candidates
            .iter()
            .find(|c| c.label == "dropped trailing stub track")
            .expect("trailing 79-frame stub must produce a drop-last stage");
        assert_eq!(dropped_last.kept_indices, (0..9).collect::<Vec<_>>());
        assert_eq!(
            super::build_mb_toc(&dropped_last.sectors).unwrap(),
            "1+9+232049+150+26702+50461+78872+97596+116395+144731+169281+194435",
            "drop-last TOC keeps the CD-accurate nine-track geometry"
        );
        // drop-all dedupes into the same TOC (the stub is the only sub-4s track).
        assert_eq!(
            candidates.len(),
            2,
            "one stub at the tail must yield exactly as-is + drop-last after dedupe"
        );
        assert_eq!(dropped_last.dropped_indices(10), vec![9]);
    }

    #[test]
    fn cascade_clean_disc_yields_single_as_is_candidate() {
        // No sub-4s tracks: cascade must not invent extra stages/requests.
        let frames = vec![26552, 23759, 28411, 18724, 18799, 28336, 24550, 25154, 37693];
        let candidates = super::toc_candidates_from_frames(&frames);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].label, "as-is");
    }

    #[test]
    fn cascade_handles_leading_and_interior_stubs() {
        // stub, real, stub(interior), real, stub(tail)
        let frames = vec![100, 20000, 200, 21000, 150];
        let candidates = super::toc_candidates_from_frames(&frames);
        let labels: Vec<&str> = candidates.iter().map(|c| c.label).collect();
        assert_eq!(
            labels,
            vec![
                "as-is",
                "dropped leading stub track",
                "dropped trailing stub track",
                "dropped edge stub tracks",
                "dropped all stub tracks",
            ]
        );
        let all_dropped = candidates.last().unwrap();
        assert_eq!(all_dropped.kept_indices, vec![1, 3]);
        assert_eq!(all_dropped.dropped_indices(5), vec![0, 2, 4]);
    }

    #[test]
    fn align_release_places_mb_tracks_at_kept_ordinals() {
        let release = super::MbRelease {
            tracks: vec![
                super::MbTrack {
                    position: 1,
                    track_id: Some("t1".into()),
                    recording_id: None,
                    artist_id: None,
                    title: "Gaslighting Abbie".into(),
                    artist: "Steely Dan".into(),
                    isrc: None,
                    length_ms: Some(354_000),
                },
                super::MbTrack {
                    position: 2,
                    track_id: Some("t2".into()),
                    recording_id: None,
                    artist_id: None,
                    title: "What a Shame About Me".into(),
                    artist: "Steely Dan".into(),
                    isrc: None,
                    length_ms: Some(317_000),
                },
            ],
            ..Default::default()
        };
        // MB tracks describe source ordinals 1 and 3 (0-based); 0 and 2 were
        // dropped stubs.
        let aligned = super::align_release_tracks_to_source(&release, &[1, 3], 4);
        assert_eq!(aligned.tracks.len(), 4);
        assert_eq!(aligned.tracks[0].title, "");
        assert_eq!(aligned.tracks[1].title, "Gaslighting Abbie");
        assert_eq!(aligned.tracks[1].position, 2);
        assert_eq!(aligned.tracks[2].title, "");
        assert_eq!(aligned.tracks[3].title, "What a Shame About Me");
        assert_eq!(aligned.tracks[3].position, 4);
    }

    use super::*;

    #[test]
    fn lucene_escape_passes_through_safe_characters() {
        assert_eq!(lucene_escape(""), "");
        assert_eq!(lucene_escape("plain text"), "plain text");
        assert_eq!(lucene_escape("Thelonious Monk"), "Thelonious Monk");
        // Apostrophes NOT special — pass through unchanged.
        assert_eq!(lucene_escape("I'm Confessin'"), "I'm Confessin'");
        // Lone & and | NOT special — only the doubled operators are.
        assert_eq!(lucene_escape("Rock & Roll"), "Rock & Roll");
        assert_eq!(lucene_escape("A|B"), "A|B");
        // Digits, periods, commas, spaces all pass through.
        assert_eq!(lucene_escape("Vol. 2, 1965"), "Vol. 2, 1965");
    }

    #[test]
    fn lucene_escape_handles_all_single_char_reserved() {
        // Reserved set: + - ! ( ) { } [ ] ^ " ~ * ? : \ /
        assert_eq!(lucene_escape("a+b"), "a\\+b");
        assert_eq!(lucene_escape("a-b"), "a\\-b");
        assert_eq!(lucene_escape("a!b"), "a\\!b");
        assert_eq!(lucene_escape("a(b)c"), "a\\(b\\)c");
        assert_eq!(lucene_escape("a{b}c"), "a\\{b\\}c");
        assert_eq!(lucene_escape("a[b]c"), "a\\[b\\]c");
        assert_eq!(lucene_escape("a^b"), "a\\^b");
        assert_eq!(lucene_escape("a\"b"), "a\\\"b");
        assert_eq!(lucene_escape("a~b"), "a\\~b");
        assert_eq!(lucene_escape("a*b"), "a\\*b");
        assert_eq!(lucene_escape("a?b"), "a\\?b");
        assert_eq!(lucene_escape("a:b"), "a\\:b");
        assert_eq!(lucene_escape("a\\b"), "a\\\\b");
        assert_eq!(lucene_escape("a/b"), "a\\/b");
    }

    #[test]
    fn lucene_escape_handles_doubled_boolean_operators() {
        // && and || are operators; escape both characters.
        assert_eq!(lucene_escape("a && b"), "a \\&\\& b");
        assert_eq!(lucene_escape("a || b"), "a \\|\\| b");
        // Triple & — first two are an operator, third is lone.
        assert_eq!(lucene_escape("a&&&b"), "a\\&\\&&b");
    }

    #[test]
    fn lucene_escape_realistic_track_title_round_trips_safely() {
        // Real titles from the user's library: parens, apostrophes,
        // colons, commas, hyphens.
        let titles = [
            "These Foolish Things (Remind Me of You)",
            "I'm Confessin' (That I Love You)",
            "I Surrender, Dear",
            "Monk's Point",
            "Bitches Brew: Disc 2",
        ];
        for t in titles {
            let escaped = lucene_escape(t);
            // No bare metacharacters left after escape, except inside
            // already-escaped sequences. Spot-check: every `(` is
            // preceded by `\`.
            for (i, c) in escaped.char_indices() {
                if matches!(
                    c,
                    '+' | '-'
                        | '!'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '['
                        | ']'
                        | '^'
                        | '"'
                        | '~'
                        | '*'
                        | '?'
                        | ':'
                        | '/'
                ) {
                    let prev = escaped[..i].chars().last();
                    assert_eq!(
                        prev,
                        Some('\\'),
                        "unescaped {} in {:?} (from {:?})",
                        c,
                        escaped,
                        t
                    );
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn mb_rate_limiter_serializes_five_calls_across_four_seconds() {
        use std::time::Duration;
        // Reset limiter state to "ready now" under the paused clock.
        // Without this, tests run in series could leave a stale
        // future-dated `next` from a prior test, skewing this run.
        {
            let mut guard = MB_NEXT_ALLOWED.lock().await;
            *guard = tokio::time::Instant::now();
        }
        let start = tokio::time::Instant::now();
        // Sequential acquires: under a paused runtime with no other
        // tasks pending, tokio auto-advances virtual time to the
        // next sleep deadline, so each `mb_acquire().await` after the
        // first costs exactly MB_MIN_INTERVAL of virtual time.
        let mut stamps = Vec::with_capacity(5);
        for _ in 0..5 {
            mb_acquire().await;
            stamps.push(tokio::time::Instant::now());
        }
        // Stamps must be strictly increasing by ≥1s each (after the
        // first, which fires immediately).
        for w in stamps.windows(2) {
            let gap = w[1].duration_since(w[0]);
            assert_eq!(
                gap, MB_MIN_INTERVAL,
                "expected exactly {:?} gap between consecutive MB calls; got {:?}",
                MB_MIN_INTERVAL, gap,
            );
        }
        // Total span across 5 calls = 4 * MB_MIN_INTERVAL.
        let span = stamps.last().unwrap().duration_since(start);
        assert_eq!(
            span,
            MB_MIN_INTERVAL * 4,
            "expected exactly 4s span for 5 serialized calls; got {:?}",
            span,
        );
        // Smoke: total elapsed virtual time should be ≥4s.
        assert!(span >= Duration::from_secs(4));
    }

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
            disc_count: 1,
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
        let sectors = vec![
            150, 19515, 36358, 51913, 72407, 112096, 134447, 193500, 280413, 293571,
        ];
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
    fn parse_mb_search_response_prefers_dvd_video_media() {
        let body = r#"{
            "releases": [
                {
                    "id": "cd-high", "score": 100, "title": "CD",
                    "media": [{ "format": "CD", "track-count": 10 }]
                },
                {
                    "id": "dvd-lower", "score": 80, "title": "DVD",
                    "media": [{ "format": "DVD-Video", "track-count": 10 }]
                }
            ]
        }"#;
        let releases = parse_mb_search_response_all(body, 10).unwrap();
        assert_eq!(releases[0].release_id, "dvd-lower");
    }

    #[test]
    fn parse_mb_detail_response_prefers_dvd_video_medium_with_matching_count() {
        let body = r#"{
            "id": "abc-123", "title": "Album",
            "media": [
                {
                    "format": "CD",
                    "track-count": 2,
                    "tracks": [
                        { "position": 1, "title": "CD 1" },
                        { "position": 2, "title": "CD 2" }
                    ]
                },
                {
                    "format": "DVD-Video",
                    "track-count": 2,
                    "tracks": [
                        { "position": 1, "title": "DVD 1" },
                        { "position": 2, "title": "DVD 2" }
                    ]
                }
            ]
        }"#;
        let release = parse_mb_detail_response(body, 2).unwrap();
        assert_eq!(release.tracks[0].title, "DVD 1");
    }

    #[test]
    fn parse_mb_response_returns_none_on_empty() {
        assert!(parse_mb_response(r#"{"releases":[]}"#, 0)
            .unwrap()
            .is_none());
        assert!(parse_mb_response(r#"{"error":"not found"}"#, 0)
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_mb_detail_response_unwraps_top_level_release() {
        let body = r#"{
            "id": "abc-123", "title": "Album",
            "artist-credit": [{ "artist": { "id": "art-1", "name": "Artist" } }],
            "media": [{
                "track-count": 2,
                "tracks": [
                    { "position": 1, "title": "T1", "length": 200000 },
                    { "position": 2, "title": "T2", "length": 250000 }
                ]
            }]
        }"#;
        let r = parse_mb_detail_response(body, 2).unwrap();
        assert_eq!(r.release_id, "abc-123");
        assert_eq!(r.tracks.len(), 2);
        assert_eq!(r.tracks[0].title, "T1");
        assert_eq!(r.tracks[1].length_ms, Some(250000));
    }

    #[test]
    fn parse_mb_detail_response_rejects_search_shape() {
        // The wrapped `{releases:[]}` shape from search/TOC should not
        // be silently accepted here — it lacks a top-level `id`.
        let body = r#"{"releases":[{"id":"a","title":"A","media":[]}]}"#;
        assert!(parse_mb_detail_response(body, 0).is_err());
    }

    #[test]
    fn parse_mb_detail_response_surfaces_error_body() {
        let body = r#"{"error":"Not Found"}"#;
        assert!(parse_mb_detail_response(body, 0).is_err());
    }

    #[test]
    fn durations_consistent_within_tolerance() {
        assert!(durations_consistent(100_000, 100_000, 3000), "exact match");
        assert!(
            durations_consistent(100_500, 100_000, 3000),
            "file slightly longer"
        );
        assert!(
            durations_consistent(99_500, 100_000, 3000),
            "file slightly shorter"
        );
        assert!(
            durations_consistent(100_000, 103_000, 3000),
            "exactly at tolerance"
        );
    }

    #[test]
    fn durations_consistent_outside_tolerance() {
        assert!(
            !durations_consistent(100_000, 60_000_000, 3000),
            "1 track of N: huge mismatch"
        );
        assert!(
            !durations_consistent(100_000, 103_001, 3000),
            "just past tolerance"
        );
        assert!(
            !durations_consistent(60_000_000, 100_000, 3000),
            "reversed direction also mismatches"
        );
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
        assert_eq!(
            r.tracks[1].length_ms,
            Some(180000),
            "recording-level fallback"
        );
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
        assert!(parse_mb_response_all(r#"{"releases":[]}"#, 0)
            .unwrap()
            .is_empty());
        assert!(parse_mb_response_all(r#"{"error":"not found"}"#, 0)
            .unwrap()
            .is_empty());
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
    fn mb_search_cache_round_trip() {
        let db = crate::db::Database::open_memory().expect("open memory db");
        let key = search_cache_key("Thelonious Monk", "Solo Monk", Some("SRGS 4520"), None);
        let body = r#"{"releases":[{"id":"x","title":"Solo Monk","media":[]}]}"#;
        assert!(db.get_cached_mb_search(&key).is_none());
        db.store_mb_search(&key, body).unwrap();
        assert_eq!(db.get_cached_mb_search(&key).as_deref(), Some(body));
    }

    #[test]
    fn mb_search_cache_namespaced_from_toc() {
        // Same string used as both a TOC entry and a search-cache entry
        // must not collide — the tables are separate.
        let db = crate::db::Database::open_memory().expect("open memory db");
        let s = "shared-string";
        db.store_mb_response(s, "toc-body").unwrap();
        assert!(db.get_cached_mb_search(s).is_none());
        db.store_mb_search(s, "search-body").unwrap();
        assert_eq!(db.get_cached_mb_response(s).as_deref(), Some("toc-body"));
        assert_eq!(db.get_cached_mb_search(s).as_deref(), Some("search-body"));
    }

    #[test]
    fn search_cache_key_is_canonical() {
        // Identical inputs collide on key (cache hit); differing
        // catalog produces a distinct key.
        let a = search_cache_key("Miles Davis", "Kind of Blue", None, Some("1959"));
        let b = search_cache_key("Miles Davis", "Kind of Blue", None, Some("1959"));
        assert_eq!(a, b);
        let with_catno =
            search_cache_key("Miles Davis", "Kind of Blue", Some("CL 1355"), Some("1959"));
        assert_ne!(a, with_catno);
        assert!(a.starts_with("search:v1:"));
    }

    #[test]
    fn detail_cache_key_namespaced() {
        // Detail keys must not collide with search keys even if the
        // search-query string happens to equal the MBID.
        let mbid = "abc-123";
        let detail = detail_cache_key(mbid);
        let search = search_cache_key(mbid, "", None, None);
        assert_ne!(detail, search);
        assert!(detail.starts_with("detail:v1:"));
    }

    #[tokio::test]
    async fn fetch_release_detail_short_circuits_on_cached_body() {
        // Pass a cached body — no HTTP / rate limiter touched.
        // (If the function were to fall through to the network path,
        // this test would either hang on the rate limiter or fail on
        // DNS depending on the environment.)
        let body = r#"{
            "id": "abc-123", "title": "Album",
            "artist-credit": [{ "artist": { "id": "art-1", "name": "Artist" } }],
            "media": [{
                "track-count": 1,
                "tracks": [{ "position": 1, "title": "T1", "length": 200000 }]
            }]
        }"#;
        let out = fetch_release_detail("abc-123", 1, Some(body.to_string()))
            .await
            .expect("cached body should parse");
        let r = out.release.expect("release present on cache hit");
        assert_eq!(r.release_id, "abc-123");
        assert_eq!(r.tracks.len(), 1);
        // Cache hit produces no cache_write (we already had it).
        assert!(out.cache_write.is_none());
    }

    #[tokio::test]
    async fn fetch_release_detail_empty_mbid_errors() {
        // Sanity: empty MBID rejected before any cache check.
        assert!(fetch_release_detail("", 0, None).await.is_err());
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

    fn empty_editor_state(n: usize) -> (crate::tui::app::MetadataEditorState, tempfile::TempDir) {
        use crate::tui::app::MetadataEditorState;
        let td = tempfile::tempdir().expect("tempdir");
        let paths: Vec<std::path::PathBuf> = (0..n)
            .map(|i| td.path().join(format!("{:02}.flac", i + 1)))
            .collect();
        let state = MetadataEditorState::for_files(
            paths,
            Vec::new(),
            (0..n).map(|i| format!("{:02}", i + 1)).collect(),
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        (state, td)
    }

    #[test]
    fn populate_sorts_entries_with_mb_keys_in_logical_positions() {
        let (mut state, _td) = empty_editor_state(2);
        let mut release = rel(
            "rid",
            vec![
                trk(1, "T1", "A", Some("USRC1")),
                trk(2, "T2", "A", Some("USRC2")),
            ],
        );
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
            state.active_surface().entries.iter().position(|e| e.display_key == key)
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
        let (mut state, _td) = empty_editor_state(2);
        // MB returns release_id only — no catalog/barcode/country/IDs/etc.
        let release = rel(
            "rid",
            vec![trk(1, "T1", "A", None), trk(2, "T2", "A", None)],
        );
        populate_editor_mb_supplemental(&mut state, &release);

        // ALBUMID gets created because release_id is non-empty.
        assert!(state.active_surface()
            .entries
            .iter()
            .any(|e| e.display_key == "MUSICBRAINZ_ALBUMID"));
        // None of these MB-only entries should exist (MB had nothing).
        for absent in [
            "ISRC",
            "MUSICBRAINZ_TRACKID",
            "MUSICBRAINZ_RELEASETRACKID",
            "MUSICBRAINZ_ARTISTID",
            "MUSICBRAINZ_ALBUMARTISTID",
            "MUSICBRAINZ_RELEASEGROUPID",
            "ORIGINALDATE",
            "RELEASECOUNTRY",
            "CATALOGNUMBER",
        ] {
            assert!(
                state.active_surface()
                    .entries
                    .iter()
                    .find(|e| e.display_key == absent)
                    .is_none(),
                "expected no {} entry but found one",
                absent,
            );
        }
    }

    #[test]
    fn populate_stamps_mb_proposed_for_changed_fields() {
        let (mut state, _td) = empty_editor_state(2);
        let mut release = rel(
            "rid",
            vec![
                trk(1, "Track 1", "Artist", Some("USRC1")),
                trk(2, "Track 2", "Artist", None),
            ],
        );
        release.title = "Album".into();
        release.year = Some("1971".into());
        populate_editor_from_mb(&mut state, &release);

        let title_entry = state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .expect("TITLE entry");
        assert!(title_entry.mb_proposed_value.is_some());
        assert_eq!(
            title_entry.mb_proposed_per_file.as_ref().unwrap(),
            &vec!["Track 1".to_string(), "Track 2".to_string()],
        );
    }

    #[test]
    fn populate_supplemental_writes_isrc_catalog_and_mb_only_fields() {
        let (mut state, _td) = empty_editor_state(2);
        let mut release = rel(
            "rid",
            vec![trk(1, "T1", "A", Some("USRC1")), trk(2, "T2", "A", None)],
        );
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
            state.active_surface()
                .entries
                .iter()
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
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .is_none());
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "ALBUM")
            .is_none());
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "DATE")
            .is_none());
    }

    #[test]
    fn populate_editor_from_mb_fills_track_and_album_fields() {
        let (mut state, _td) = empty_editor_state(2);
        let mut release = rel(
            "x",
            vec![
                trk(1, "Track 1", "Artist", Some("USRC17607839")),
                trk(2, "Track 2", "Artist", None),
            ],
        );
        release.title = "Album".into();
        release.year = Some("1971".into());
        release.catalog = Some("UICY-94626".into());
        release.barcode = Some("0044007735428".into());
        populate_editor_from_mb(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.active_surface()
                .entries
                .iter()
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
        assert!(state.active_surface().dirty);
    }

    #[test]
    fn populate_editor_from_mb_single_image_per_track_titles_artists_isrc() {
        // Per-track populate fires only when single_image guards pass
        // (Phase 5: same gate as the populate-time CUESHEET embed —
        // verifiable identity / duration). Install silence.flac (100ms)
        // and pick MB lengths summing within ±3s so the duration
        // verifier passes. Then per-track TITLE / ARTIST / ISRC populate
        // to dim = release.tracks.len(); ALBUM / DATE stay dim 1. The
        // MB-only per-track IDs (RECORDINGID / RELEASETRACKID /
        // ARTISTID) have no CUESHEET home so they stay album-only and
        // are not created on single-image.
        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "Lead-off Track", "Artist A", Some("USRC17607839"));
                    t.length_ms = Some(40);
                    t.recording_id = Some("rec1".into());
                    t.track_id = Some("tk1".into());
                    t.artist_id = Some("artid1".into());
                    t
                },
                {
                    let mut t = trk(2, "Second Track", "Artist B", None);
                    t.length_ms = Some(30);
                    t
                },
                {
                    let mut t = trk(3, "Third Track", "Artist C", None);
                    t.length_ms = Some(30);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        release.artist = "Album Artist".into();
        release.year = Some("1970".into());
        populate_editor_from_mb(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(key))
                .map(|e| e.per_file_values.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            lookup("TITLE"),
            vec!["Lead-off Track", "Second Track", "Third Track"],
            "TITLE must be per-track on per-track-eligible single-image"
        );
        assert_eq!(
            lookup("ARTIST"),
            vec!["Artist A", "Artist B", "Artist C"],
            "ARTIST must be per-track on per-track-eligible single-image"
        );
        assert_eq!(
            lookup("ALBUM"),
            vec!["Whole Album"],
            "ALBUM stays album-level dim 1"
        );
        assert_eq!(lookup("DATE"), vec!["1970"]);
        assert_eq!(
            lookup("ISRC"),
            vec!["USRC17607839", "", ""],
            "ISRC must be per-track on per-track-eligible single-image (Phase 1b)"
        );
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "MUSICBRAINZ_TRACKID")
            .is_none());
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "MUSICBRAINZ_RELEASETRACKID")
            .is_none());
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "MUSICBRAINZ_ARTISTID")
            .is_none());
    }

    #[test]
    fn is_per_track_eligible_returns_false_for_not_applicable_cases() {
        // Regression for the 7c60aa1 refactor: the boolean wrapper
        // must return false for multi-file and single-track-release
        // cases. per_track_skip_reason returns None for those (no
        // status message needed), but the boolean must NOT be true.
        let release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "T", "A", None);
                    t.length_ms = Some(50);
                    t
                },
                {
                    let mut t = trk(2, "T", "A", None);
                    t.length_ms = Some(50);
                    t
                },
            ],
        );
        let paths_multi = vec![
            std::path::PathBuf::from("/tmp/a.flac"),
            std::path::PathBuf::from("/tmp/b.flac"),
        ];
        assert!(
            !is_per_track_eligible(&paths_multi, &release, false),
            "multi-file → not eligible"
        );
        let single_track = rel(
            "rid",
            vec![{
                let mut t = trk(1, "T", "A", None);
                t.length_ms = Some(100);
                t
            }],
        );
        assert!(
            !is_per_track_eligible(
                &[std::path::PathBuf::from("/tmp/a.flac")],
                &single_track,
                false
            ),
            "single-track release → not eligible"
        );
    }

    #[test]
    fn count_mismatch_text_equal_counts_no_warning() {
        assert!(count_mismatch_text(12, 12).is_none());
        assert!(count_mismatch_text(1, 1).is_none());
        assert!(count_mismatch_text(0, 0).is_none());
    }

    #[test]
    fn count_mismatch_text_multi_file_mismatch_warns() {
        let msg = count_mismatch_text(12, 14).expect("warn");
        assert!(msg.contains("14 tracks"));
        assert!(msg.contains("editor has 12"));
    }

    #[test]
    fn count_mismatch_text_singular_track_word() {
        let msg = count_mismatch_text(2, 1).expect("warn");
        assert!(msg.contains("1 track,"), "got: {}", msg);
        assert!(!msg.contains("1 tracks"), "got: {}", msg);
    }

    #[test]
    fn count_mismatch_text_zero_mb_tracks_warns() {
        // Edge: malformed release with no tracks. Differs from
        // n_files, so fires. Content is "0 tracks, editor has N" —
        // user sees that nothing was matched.
        let msg = count_mismatch_text(5, 0).expect("warn");
        assert!(msg.contains("0 tracks"));
        assert!(msg.contains("editor has 5"));
    }

    #[test]
    fn dvdv_duration_mismatches_warn_over_tolerance() {
        let mut release = rel(
            "rid",
            vec![
                trk(1, "One", "Artist", None),
                trk(2, "Two", "Artist", None),
            ],
        );
        release.tracks[0].length_ms = Some(240_000);
        release.tracks[1].length_ms = Some(210_000);
        let mismatches = dvdv_duration_mismatches(&[240.0, 201.0], &release, 5_000);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].track_number, 2);
        assert_eq!(mismatches[0].diff_ms, 9_000);
    }

    #[test]
    fn apply_dvdv_duration_warnings_adds_editor_row() {
        let (mut state, _td) = empty_editor_state(2);
        state.active_surface_mut().dvdv_track_durations = Some(vec![240.0, 201.0]);
        let mut release = rel(
            "rid",
            vec![
                trk(1, "One", "Artist", None),
                trk(2, "Two", "Artist", None),
            ],
        );
        release.tracks[0].length_ms = Some(240_000);
        release.tracks[1].length_ms = Some(210_000);

        let msg = apply_dvdv_duration_warnings(&mut state, &release).expect("warning");
        assert!(msg.contains("DVD-Video duration warning"));
        let entry = state.active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == DVDV_DURATION_WARNING_KEY)
            .expect("duration warning row");
        assert!(entry.per_file_values[0].is_empty());
        assert!(entry.per_file_values[1].contains("DVD 3:21 vs MB 3:30"));
    }

    #[test]
    fn track_count_mismatch_message_single_image_no_warning() {
        // paths.len() == 1 with N>1 MB tracks is the legitimate
        // single-image rip case (titles ride in the CUESHEET tag).
        // Helper returns None.
        use crate::tui::app::MetadataEditorState;
        let state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/x.flac")],
            Vec::new(),
            vec!["01".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        let release = rel(
            "rid",
            vec![
                trk(1, "T1", "A", None),
                trk(2, "T2", "A", None),
                trk(3, "T3", "A", None),
            ],
        );
        assert!(track_count_mismatch_message(&state, &release).is_none());
    }

    #[test]
    fn track_count_mismatch_message_single_image_single_track_no_warning() {
        // paths.len() == 1, n_mb == 1: genuine 1:1 match, not a
        // mismatch and not a single-image case. No warning.
        use crate::tui::app::MetadataEditorState;
        let state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/x.flac")],
            Vec::new(),
            vec!["01".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        let release = rel("rid", vec![trk(1, "T", "A", None)]);
        assert!(track_count_mismatch_message(&state, &release).is_none());
    }

    #[test]
    fn track_count_mismatch_message_multi_file_mismatch_warns() {
        use crate::tui::app::MetadataEditorState;
        let state = MetadataEditorState::for_files(
            vec![
                std::path::PathBuf::from("/tmp/01.flac"),
                std::path::PathBuf::from("/tmp/02.flac"),
                std::path::PathBuf::from("/tmp/03.flac"),
            ],
            Vec::new(),
            vec!["01".into(), "02".into(), "03".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        let release = rel(
            "rid",
            vec![
                trk(1, "T1", "A", None),
                trk(2, "T2", "A", None),
                trk(3, "T3", "A", None),
                trk(4, "T4", "A", None),
                trk(5, "T5", "A", None),
            ],
        );
        let msg = track_count_mismatch_message(&state, &release).expect("warn");
        assert!(msg.contains("5 tracks"));
        assert!(msg.contains("editor has 3"));
    }

    #[test]
    fn per_track_skip_reason_returns_messages_for_each_skip_path() {
        // None-cases: not single-image, single-track release.
        let paths_multi = vec![
            std::path::PathBuf::from("/tmp/a.flac"),
            std::path::PathBuf::from("/tmp/b.flac"),
        ];
        let release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "T", "A", None);
                    t.length_ms = Some(50);
                    t
                },
                {
                    let mut t = trk(2, "T", "A", None);
                    t.length_ms = Some(50);
                    t
                },
            ],
        );
        assert!(
            per_track_skip_reason(&paths_multi, &release).is_none(),
            "multi-file: no per-track expectation, no skip message"
        );

        let single_track_release = rel("rid", vec![trk(1, "T", "A", None)]);
        assert!(
            per_track_skip_reason(
                &[std::path::PathBuf::from("/tmp/a.flac")],
                &single_track_release
            )
            .is_none(),
            "single-track release: not a per-track-applicable case"
        );

        // Multi-disc skip.
        let mut multi_disc = release.clone();
        multi_disc.disc_count = 2;
        let r =
            per_track_skip_reason(&[std::path::PathBuf::from("/tmp/a.flac")], &multi_disc).unwrap();
        assert!(r.contains("multi-disc"));
        assert!(r.contains("2 discs"));

        // Unverifiable skip (no fixture installed → probe fails).
        let r = per_track_skip_reason(
            &[std::path::PathBuf::from("/tmp/nonexistent.flac")],
            &release,
        )
        .unwrap();
        assert!(r.contains("can't verify") || r.contains("album-level"));

        // Missing-lengths skip (need a fixture for identity verification
        // to pass; quick manual test of the lengths-missing branch:
        // unverifiable beats lengths-missing in the order; covered by
        // the existing _identity_matches_but_no_lengths integration test).
    }

    #[test]
    fn populate_editor_from_mb_single_image_album_fallback_when_unverifiable() {
        // Single-image rip with no fixture installed → probe fails →
        // verify_single_image_matches_release returns a skip reason →
        // is_per_track_eligible is false → album-level fallback fires.
        // TITLE gets the album title (not track 1's), ARTIST gets the
        // album artist, ISRC entry is not created at all.
        let (mut state, _td) = empty_editor_state(1);
        let mut release = rel(
            "rid",
            vec![
                trk(1, "Lead-off Track", "Artist A", Some("USRC17607839")),
                trk(2, "Second Track", "Artist B", None),
                trk(3, "Third Track", "Artist C", None),
            ],
        );
        release.title = "Whole Album".into();
        release.artist = "Album Artist".into();
        populate_editor_from_mb(&mut state, &release);

        let lookup = |key: &str| -> Vec<String> {
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(key))
                .map(|e| e.per_file_values.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            lookup("TITLE"),
            vec!["Whole Album"],
            "non-eligible single-image: TITLE falls back to album title"
        );
        assert_eq!(lookup("ARTIST"), vec!["Album Artist"]);
        assert_eq!(lookup("ALBUM"), vec!["Whole Album"]);
        // ISRC must NOT be created (no per-track CUESHEET home, and
        // album-level ISRC is meaningless).
        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key == "ISRC")
                .is_none(),
            "non-eligible single-image: ISRC must not be created"
        );
        // No CUESHEET embed either (per_track_populate gates that too).
        assert!(state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .is_none());
    }

    #[test]
    fn populate_editor_from_mb_single_image_creates_cuesheet_when_lengths_present() {
        // Single-image rip with MB providing track lengths → CUESHEET
        // tag entry should be auto-created with the per-track listing
        // baked into a multi-line CUE string.
        //
        // Under strict verification we install the silence.flac fixture
        // (100ms total) and pick MB lengths that sum within tolerance,
        // exercising the duration-match path. Cumulative-timestamp
        // correctness is verified independently in cue_generate.rs.
        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(50);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(50);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        release.artist = "Album Artist".into();
        populate_editor_from_mb(&mut state, &release);

        let cue_entry = state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
            .expect("CUESHEET entry should be auto-created on single-image populate");
        assert!(cue_entry.is_binary, "CUESHEET row must block inline edit");
        let cue = &cue_entry.per_file_values[0];
        assert!(cue.contains("FILE \"01.flac\" FLAC"));
        assert!(cue.contains("First"));
        assert!(cue.contains("Second"));
    }

    #[test]
    fn populate_editor_from_mb_single_image_skips_cuesheet_when_lengths_missing() {
        // No MB lengths → CUESHEET must NOT be created (silent
        // corruption is worse than a missing tag the user can request
        // manually later).
        let (mut state, _td) = empty_editor_state(1);
        let mut release = rel(
            "rid",
            vec![
                trk(1, "First", "Artist", None),
                trk(2, "Second", "Artist", None),
            ],
        );
        release.title = "Whole Album".into();
        release.artist = "Album Artist".into();
        populate_editor_from_mb(&mut state, &release);

        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "CUESHEET must not be created when track lengths are missing",
        );
    }

    /// Copy `tests/fixtures/silence.flac` into the test's tempdir at
    /// the path `state.active_surface().paths[0]` points to. The fixture is 100ms.
    fn install_silence_at(state: &crate::tui::app::MetadataEditorState) {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.flac");
        let dst = &state.active_surface().paths[0];
        std::fs::copy(&fixture, dst).expect("install silence.flac fixture");
    }

    #[test]
    fn populate_editor_from_mb_single_image_skips_cuesheet_when_durations_mismatch() {
        // File is 100ms (silence.flac). MB tracks summing to 60_000ms
        // (60s) — far outside ±3s tolerance → skip.
        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(30_000);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(30_000);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        populate_editor_from_mb(&mut state, &release);
        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "duration mismatch (100ms vs 60s) must skip CUESHEET embed",
        );
    }

    #[test]
    fn populate_editor_from_mb_single_image_embeds_when_identity_matches_despite_duration_mismatch()
    {
        // File is 100ms but carries MUSICBRAINZ_ALBUMID matching the
        // release. Identity is positive evidence the file belongs to
        // this release, so we embed even though duration check would
        // otherwise refuse on the 60s/100ms divergence. (Realistic
        // scenario: a corrupt or in-progress file that lofty can still
        // tag-read but ffmpeg can't probe — though here the file is
        // fine; we just construct a deliberate duration mismatch to
        // prove the identity path overrides.)
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        // Write MUSICBRAINZ_ALBUMID = "matching-rid" into the file.
        let path = &state.active_surface().paths[0];
        {
            let mut tagged = lofty::read_from_path(path).expect("read fixture copy");
            if tagged.primary_tag().is_none() {
                let tt = tagged.primary_tag_type();
                tagged.insert_tag(lofty::tag::Tag::new(tt));
            }
            let tag = tagged.primary_tag_mut().expect("primary tag");
            tag.insert_unchecked(TagItem::new(
                ItemKey::MusicBrainzReleaseId,
                ItemValue::Text("matching-rid".to_string()),
            ));
            tagged
                .save_to_path(path, WriteOptions::default())
                .expect("save tag");
        }

        let mut release = rel(
            "matching-rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(30_000);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(30_000);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        populate_editor_from_mb(&mut state, &release);

        assert!(
            state.active_surface()
                .entries
                .iter()
                .any(|e| e.display_key.eq_ignore_ascii_case("CUESHEET")),
            "matching MUSICBRAINZ_ALBUMID must override duration mismatch and embed CUESHEET",
        );
    }

    #[test]
    fn populate_editor_from_mb_single_image_album_fallback_when_identity_matches_but_no_lengths() {
        // Phase B: even when identity verifies (matching MUSICBRAINZ_
        // ALBUMID), if MB hasn't supplied per-track lengths we can't
        // generate a CUESHEET. Eligibility tightened to refuse this
        // corner — user gets the album-level fallback (TITLE = album
        // name) instead of per-track entries with no anchor that
        // Phase 4 would refuse to save.
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        let path = &state.active_surface().paths[0];
        {
            let mut tagged = lofty::read_from_path(path).expect("read fixture");
            if tagged.primary_tag().is_none() {
                let tt = tagged.primary_tag_type();
                tagged.insert_tag(lofty::tag::Tag::new(tt));
            }
            let tag = tagged.primary_tag_mut().expect("primary tag");
            tag.insert_unchecked(TagItem::new(
                ItemKey::MusicBrainzReleaseId,
                ItemValue::Text("matching-rid".into()),
            ));
            tagged
                .save_to_path(path, WriteOptions::default())
                .expect("save tag");
        }
        // Identity matches BUT no track lengths.
        let mut release = rel(
            "matching-rid",
            vec![
                trk(1, "First", "Artist", None),
                trk(2, "Second", "Artist", None),
            ],
        );
        release.title = "Whole Album".into();
        populate_editor_from_mb(&mut state, &release);

        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "no track lengths → no CUESHEET even with identity match",
        );
        let title = state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key.eq_ignore_ascii_case("TITLE"))
            .unwrap();
        assert_eq!(
            title.per_file_values,
            vec!["Whole Album"],
            "album-level fallback: TITLE = album name, dim 1"
        );
    }

    #[test]
    fn populate_editor_from_mb_single_image_skips_cuesheet_when_unverifiable() {
        // No file installed (probe fails) AND no MUSICBRAINZ_ALBUMID
        // tag (no identity anchor). Strict policy: refuse to embed.
        // Previously this case embedded permissively; now it doesn't.
        let (mut state, _td) = empty_editor_state(1);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(240_000);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(180_000);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        populate_editor_from_mb(&mut state, &release);
        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "unverifiable file (no probe + no identity) must skip CUESHEET embed strictly",
        );
    }

    #[test]
    fn populate_editor_from_mb_single_image_skips_cuesheet_when_multidisc() {
        // disc_count > 1: don't embed (single file can't unambiguously
        // represent a multi-disc release).
        let (mut state, _td) = empty_editor_state(1);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(240_000);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(180_000);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        release.disc_count = 2;
        populate_editor_from_mb(&mut state, &release);

        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "multi-disc release must not auto-embed CUESHEET",
        );
    }

    #[test]
    fn populate_editor_from_mb_single_image_preserves_existing_cuesheet() {
        // Phase 5 guard: when state.active_surface().entries already contains a CUESHEET
        // (whether parsed from an embedded tag at open or injected from
        // a sidecar by Phase 2's editor-open flow), populate must NOT
        // rebuild it from MB. Phase 4 will mutate the existing entry
        // in place at save time using user edits + β album re-derive.
        //
        // Replaces the older `_skips_cuesheet_when_sidecar_present`
        // test: with the sidecar guard dropped, sidecar presence no
        // longer affects populate directly. The semantically equivalent
        // assertion is now "existing CUESHEET in state isn't overwritten."
        let (mut state, _td) = empty_editor_state(1);
        install_silence_at(&state);
        let pre_existing = "TITLE \"Pre-existing\"\n\
                            FILE \"x.flac\" FLAC\n\
                            TRACK 01 AUDIO\nTITLE \"Pre T1\"\nINDEX 01 00:00:00\n\
                            TRACK 02 AUDIO\nTITLE \"Pre T2\"\nINDEX 01 00:00:50\n";
        state.active_surface_mut().entries.push(crate::tui::probe::TagEntry {
            display_key: "CUESHEET".to_string(),
            item_key: lofty::tag::ItemKey::Unknown("CUESHEET".to_string()),
            value: "<cue summary>".to_string(),
            original: String::new(),
            is_binary: true,
            is_mixed: false,
            per_file_values: vec![pre_existing.to_string()],
            per_file_originals: vec![String::new()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });

        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "MB Track 1", "Artist", None);
                    t.length_ms = Some(50);
                    t
                },
                {
                    let mut t = trk(2, "MB Track 2", "Artist", None);
                    t.length_ms = Some(50);
                    t
                },
            ],
        );
        release.title = "Whole Album".into();
        populate_editor_from_mb(&mut state, &release);

        // CUESHEET preserved verbatim — populate did NOT rebuild it.
        let cue = state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
            .expect("CUESHEET still in state");
        assert_eq!(
            cue.per_file_values[0], pre_existing,
            "existing CUESHEET preserved by populate (not regenerated from MB)"
        );
        // Per-track populate from MB still ran on TITLE.
        let title = state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key.eq_ignore_ascii_case("TITLE"))
            .expect("TITLE created");
        assert_eq!(
            title.per_file_values,
            vec!["MB Track 1", "MB Track 2"],
            "per-track populate fired despite an existing CUESHEET"
        );
    }

    #[test]
    fn populate_editor_from_mb_multi_file_does_not_create_cuesheet() {
        // 2 files, 2 tracks → not single-image; CUESHEET shouldn't be
        // generated (it only applies to single-image rips).
        let (mut state, _td) = empty_editor_state(2);
        let mut release = rel(
            "rid",
            vec![
                {
                    let mut t = trk(1, "First", "Artist", None);
                    t.length_ms = Some(240_000);
                    t
                },
                {
                    let mut t = trk(2, "Second", "Artist", None);
                    t.length_ms = Some(180_000);
                    t
                },
            ],
        );
        release.title = "Album".into();
        populate_editor_from_mb(&mut state, &release);

        assert!(
            state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
                .is_none(),
            "CUESHEET should only fire on single-image rips",
        );
    }

    #[test]
    fn artist_credit_joinphrase_preserved() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[
            {"name": "Foo", "joinphrase": " & "},
            {"name": "Bar"}
        ]"#,
        )
        .unwrap();
        assert_eq!(artist_credit_string(Some(&v)), "Foo & Bar");
    }
}
