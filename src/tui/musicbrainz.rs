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

use crate::db::Database;

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

/// Look up the highest-scoring MusicBrainz release matching the supplied
/// TOC. On cache hit, returns the cached parse without an HTTP call.
/// `None` means "no release matched"; `Err(_)` means a transport/parse
/// failure the caller should surface.
pub async fn lookup_release_by_toc(
    db: &Database,
    sectors: &[u32],
) -> Result<Option<MbRelease>, String> {
    let toc = build_mb_toc(sectors)
        .ok_or_else(|| "TOC must have at least 2 sector entries".to_string())?;
    let n_tracks = sectors.len() - 1;

    if let Some(json) = db.get_cached_mb_response(&toc) {
        return parse_mb_response(&json, n_tracks);
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
        // Cache the negative response too so we don't re-query on retry.
        let _ = db.store_mb_response(&toc, &body);
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("MusicBrainz returned HTTP {}", status));
    }

    let _ = db.store_mb_response(&toc, &body);
    parse_mb_response(&body, n_tracks)
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
    fn artist_credit_joinphrase_preserved() {
        let v: serde_json::Value = serde_json::from_str(r#"[
            {"name": "Foo", "joinphrase": " & "},
            {"name": "Bar"}
        ]"#).unwrap();
        assert_eq!(artist_credit_string(Some(&v)), "Foo & Bar");
    }
}
