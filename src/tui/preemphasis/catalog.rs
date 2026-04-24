//! Catalog-number-based pre-emphasis detection.
//!
//! Catalog numbers are gathered from audio tags and folder names, normalized,
//! and matched against:
//! 1. A database of exact known-PE catalog numbers (Detected)
//! 2. Catalog-number families associated with possible PE pressings (Possible)
//!
//! Source: https://www.studio-nibble.com/cd/index.php?title=Pre-emphasis_(release_list)

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lazy_static::lazy_static;
use regex::Regex;

use super::metadata::PreemphasisEvidence;
use super::PreemphasisConfidence;

/// Result of catalog-based PE detection.
#[derive(Debug, Clone)]
pub struct CatalogMatch {
    pub evidence: PreemphasisEvidence,
    pub confidence: PreemphasisConfidence,
    pub catalog_number: String,
    pub series_name: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CatalogSource {
    Tag,
    Folder,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct CatalogCandidate {
    raw: String,
    normalized: String,
    source: CatalogSource,
}

impl CatalogSource {
    fn label(self) -> &'static str {
        match self {
            CatalogSource::Tag => "audio tag",
            CatalogSource::Folder => "folder name",
        }
    }
}

// -- Catalog extraction -------------------------------------------------------

lazy_static! {
    /// Catalog-like tokens found in tag text or folder names.
    ///
    /// The regex finds candidates only. Every hit is checked for text
    /// boundaries, normalized, deduplicated, and then matched against exact
    /// catalog data or normalized series patterns.
    static ref CATALOG_CANDIDATE: Regex = Regex::new(concat!(
        r"(?ix)(",
        // Long or more specific prefixes first.
        r"35[\s·.]?8P[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|CDEPC[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|CDCBS[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|FIEND[-\s·.]*CD[-\s·.]?[0-9]+",
        r"|ROTA[-\s·.]*CD[-\s·.]?[0-9]+",
        r"|DIX[-\s·.]*CD[-\s·.]?[0-9]+",
        r"|LXHCD[-\s·.]?[0-9]+",
        r"|CDID[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|CDFA[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|CDP[-\s·.]?[0-9](?:[-\s·.]?[0-9]){5,6}",
        r"|MCAD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|MCD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}[-\s·.]?MD",
        r"|JRCD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|SNEG[-\s·.]?[0-9](?:[-\s·.]?[0-9]){2}",
        r"|FACD[-\s·.]?[0-9]+",
        r"|AGCD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){2,3}",
        r"|CSCS[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|SRCS[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|ESCA[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|CP35[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|CP32[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|35DP[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|50DP[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|32DP[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|35DC[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|56DC[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|35DH[-\s·.]?[0-9](?:[-\s·.]?[0-9]){0,2}",
        r"|38XB[-\s·.]?[0-9]+",
        r"|32PD[-\s·.]?[0-9]+",
        r"|20VD[-\s·.]?[0-9]+",
        r"|32JC[-\s·.]?[0-9]+",
        r"|CDV[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3,4}",
        r"|CCD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|BCD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|VJD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|VSD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        r"|ND[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|CK[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|EK[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|ZK[-\s·.]?[0-9](?:[-\s·.]?[0-9]){4}",
        r"|CD[-\s·.]?[0-9](?:[-\s·.]?[0-9]){3}",
        // Numeric-only catalog numbers are kept only when their normalized
        // token exists in the exact catalog map. The optional suffix covers
        // forms such as 8296632Y1 without creating partial matches.
        r"|[0-9](?:[-\s.]?[0-9]){3,}(?:[-\s.]?[A-Z][0-9]?)?",
        r")"
    ))
    .unwrap();
}

/// Normalize a catalog number for database lookup.
/// Removes punctuation and spacing, then uppercases ASCII letters.
fn normalize_catalog(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn previous_char(text: &str, byte_index: usize) -> Option<char> {
    text[..byte_index].chars().next_back()
}

fn next_char(text: &str, byte_index: usize) -> Option<char> {
    text[byte_index..].chars().next()
}

fn next_char_after_first(text: &str, byte_index: usize) -> Option<char> {
    let mut chars = text[byte_index..].chars();
    chars.next()?;
    chars.next()
}

fn next_ascii_token(text: &str, byte_index: usize) -> Option<&str> {
    let mut token_start = None;

    for (offset, c) in text[byte_index..].char_indices() {
        if !c.is_whitespace() {
            token_start = Some(byte_index + offset);
            break;
        }
    }

    let start = token_start?;

    if !text[start..].chars().next()?.is_ascii_alphanumeric() {
        return None;
    }

    let mut end = text.len();
    for (offset, c) in text[start..].char_indices() {
        if !c.is_ascii_alphanumeric() {
            end = start + offset;
            break;
        }
    }

    Some(&text[start..end])
}

fn is_probable_cd_year(token: &str) -> bool {
    token.len() == 4
        && token.as_bytes().iter().all(|b| b.is_ascii_digit())
        && (&token[..2] == "19" || &token[..2] == "20")
}

fn starts_with_ascii_digit(token: &str) -> bool {
    token.as_bytes().first().map_or(false, |b| b.is_ascii_digit())
}

fn is_probable_space_separated_suffix(token: &str) -> bool {
    let bytes = token.as_bytes();

    match bytes.len() {
        // Common matrix/suffix notation after a catalog can be written as a
        // separate single letter: "35DP-25 A". Treat that as continuation
        // rather than ordinary prose.
        1 => bytes[0].is_ascii_alphabetic(),
        // Also reject compact letter-digit or digit-letter suffixes such as
        // "A1" or "1A" without rejecting two-letter country/context tokens
        // like "UK".
        2 => {
            bytes.iter().all(|b| b.is_ascii_alphanumeric())
                && bytes.iter().any(|b| b.is_ascii_alphabetic())
                && bytes.iter().any(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

fn has_catalog_boundaries(text: &str, start: usize, end: usize) -> bool {
    let before_ok = previous_char(text, start).map_or(true, |c| !c.is_ascii_alphanumeric());
    if !before_ok {
        return false;
    }

    match next_char(text, end) {
        None => true,
        Some(c) if c.is_ascii_alphanumeric() => false,
        // Reject immediate suffixes such as CDV-2192.1 and CDV-2192-A.
        // Treat punctuation followed by whitespace as terminal punctuation.
        Some('-' | '.' | '·') => match next_char_after_first(text, end) {
            None => true,
            Some(c) if c.is_whitespace() => true,
            Some(c) => !c.is_ascii_alphanumeric(),
        },
        // Reject whitespace-separated numeric continuations such as 35DP 123 4
        // and short suffix codes such as 35DP-25 A or 35DP-25 A1. Allow
        // ordinary release-year context such as 35DP-25 1982 Japan.
        Some(c) if c.is_whitespace() => match next_ascii_token(text, end) {
            None => true,
            Some(token) if is_probable_cd_year(token) => true,
            Some(token) if is_probable_space_separated_suffix(token) => false,
            Some(token) => !starts_with_ascii_digit(token),
        },
        Some(_) => true,
    }
}

fn trimmed_match_end_before_probable_year(text: &str, start: usize, end: usize) -> Option<usize> {
    for (offset, c) in text[start..end].char_indices().rev() {
        if !c.is_whitespace() {
            continue;
        }

        let candidate_end = start + offset;
        let candidate_raw = text[start..candidate_end].trim_end();
        if candidate_raw.is_empty() {
            continue;
        }

        let Some(token) = next_ascii_token(text, candidate_end) else {
            continue;
        };
        if !is_probable_cd_year(token) {
            continue;
        }

        if candidate_is_usable(&normalize_catalog(candidate_raw)) {
            return Some(candidate_end);
        }
    }

    None
}

fn has_trailing_short_suffix_on_usable_base(raw: &str, normalized: &str) -> bool {
    let raw = raw.trim();

    for (offset, c) in raw.char_indices().rev() {
        if !(c.is_whitespace() || matches!(c, '-' | '.' | '·')) {
            continue;
        }

        let suffix_start = offset + c.len_utf8();
        let suffix = raw[suffix_start..].trim();
        if suffix.len() != 1 || !suffix.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return false;
        }

        let base = raw[..offset].trim_end();
        if base.is_empty() {
            return false;
        }

        let base_normalized = normalize_catalog(base);
        if base_normalized == normalized || base_normalized.is_empty() {
            return false;
        }

        return candidate_is_usable(&base_normalized);
    }

    false
}

fn candidate_is_usable(normalized: &str) -> bool {
    if normalized.is_empty() {
        return false;
    }

    KNOWN_PE_EXACT.contains_key(normalized)
        || KNOWN_PE_SERIES
            .iter()
            .any(|(pattern, _)| pattern.is_match(normalized))
}

fn push_candidate(
    candidates: &mut Vec<CatalogCandidate>,
    seen: &mut HashSet<String>,
    raw: &str,
    source: CatalogSource,
) {
    let raw = raw.trim();
    let normalized = normalize_catalog(raw);

    if has_trailing_short_suffix_on_usable_base(raw, &normalized) {
        return;
    }

    if !candidate_is_usable(&normalized) {
        return;
    }

    if seen.insert(normalized.clone()) {
        candidates.push(CatalogCandidate {
            raw: raw.to_string(),
            normalized,
            source,
        });
    }
}

fn extract_catalog_candidates_from_text(text: &str, source: CatalogSource) -> Vec<CatalogCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for hit in CATALOG_CANDIDATE.find_iter(text) {
        if has_catalog_boundaries(text, hit.start(), hit.end()) {
            push_candidate(&mut candidates, &mut seen, hit.as_str(), source);
            continue;
        }

        // Some family patterns allow grouped digits. In a folder like
        // "Asia 35DP-25 1982 Japan", a greedy match may consume the first
        // digit of the year as "35DP-25 1". Recover the shorter candidate
        // only when the skipped text is a plausible CD-era year and the
        // shorter normalized catalog is known.
        if let Some(trimmed_end) =
            trimmed_match_end_before_probable_year(text, hit.start(), hit.end())
        {
            if has_catalog_boundaries(text, hit.start(), trimmed_end) {
                push_candidate(
                    &mut candidates,
                    &mut seen,
                    &text[hit.start()..trimmed_end],
                    source,
                );
            }
        }
    }

    candidates
}

fn tag_value_candidates(value: &str, source: CatalogSource) -> Vec<CatalogCandidate> {
    let mut candidates = extract_catalog_candidates_from_text(value, source);

    // Also check the full tag value after normalization. This covers catalog
    // values whose punctuation is unusual but whose normalized value is known.
    let mut seen = candidates
        .iter()
        .map(|candidate| candidate.normalized.clone())
        .collect::<HashSet<_>>();
    push_candidate(&mut candidates, &mut seen, value, source);

    candidates
}

fn catalog_candidates_from_folder(audio_path: &Path) -> Vec<CatalogCandidate> {
    let Some(parent) = audio_path.parent() else {
        return Vec::new();
    };
    let Some(folder_name) = parent.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };

    extract_catalog_candidates_from_text(folder_name, CatalogSource::Folder)
}

fn catalog_candidates_from_tags(audio_path: &Path) -> Vec<CatalogCandidate> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let Ok(tagged) = lofty::read_from_path(audio_path) else {
        return Vec::new();
    };

    // Unknown tag keys are matched by exact key string in lofty, so keep a
    // broad set of common spelling and case variants.
    let freeform_keys = [
        "CATALOGNUMBER",
        "CatalogNumber",
        "catalognumber",
        "CATALOG_NUMBER",
        "Catalog_Number",
        "catalog_number",
        "CATALOG NO",
        "Catalog No",
        "catalog no",
        "CATALOGNO",
        "CatalogNo",
        "catalogno",
        "CATALOG #",
        "Catalog #",
        "catalog #",
        "CATALOG",
        "Catalog",
        "catalog",
        "CATNO",
        "CatNo",
        "catno",
        "CAT NO",
        "Cat No",
        "cat no",
        "CATALOGUE NUMBER",
        "Catalogue Number",
        "catalogue number",
        "CATALOGUE NO",
        "Catalogue No",
        "catalogue no",
        "CATALOGUE_NO",
        "Catalogue_No",
        "catalogue_no",
        "CATALOGUE",
        "Catalogue",
        "catalogue",
    ];

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for tag in tagged.tags() {
        let mut values = Vec::new();

        if let Some(value) = tag.get_string(&ItemKey::CatalogNumber) {
            values.push(value);
        }

        for key in freeform_keys {
            if let Some(value) = tag.get_string(&ItemKey::Unknown(key.to_string())) {
                values.push(value);
            }
        }

        for value in values {
            for candidate in tag_value_candidates(value, CatalogSource::Tag) {
                if seen.insert(candidate.normalized.clone()) {
                    candidates.push(candidate);
                }
            }
        }
    }

    candidates
}

fn catalog_candidates(audio_path: &Path) -> Vec<CatalogCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for candidate in catalog_candidates_from_tags(audio_path)
        .into_iter()
        .chain(catalog_candidates_from_folder(audio_path))
    {
        if seen.insert(candidate.normalized.clone()) {
            candidates.push(candidate);
        }
    }

    candidates
}

// -- Public API ---------------------------------------------------------------

/// Check if the audio file's catalog number matches a known PE pressing.
///
/// Returns:
/// - `CatalogExact` + `Detected` for exact catalog matches
/// - `CatalogSeries` + `Possible` for catalog numbers in known PE series
/// - `None` if no catalog match
pub fn check_catalog_evidence(audio_path: &Path) -> Option<CatalogMatch> {
    let candidates = catalog_candidates(audio_path);
    best_catalog_match(&candidates)
}

fn best_catalog_match(candidates: &[CatalogCandidate]) -> Option<CatalogMatch> {
    // Confidence wins over source order: any exact catalog match beats any
    // series-family match, even when the series candidate appeared earlier.
    // Within the same confidence tier, preserve candidate discovery order.
    candidates
        .iter()
        .find_map(match_exact_catalog_candidate)
        .or_else(|| candidates.iter().find_map(match_series_catalog_candidate))
}

fn match_catalog_candidate(candidate: CatalogCandidate) -> Option<CatalogMatch> {
    match_exact_catalog_candidate(&candidate).or_else(|| match_series_catalog_candidate(&candidate))
}

fn match_exact_catalog_candidate(candidate: &CatalogCandidate) -> Option<CatalogMatch> {
    let &(artist, title) = KNOWN_PE_EXACT.get(candidate.normalized.as_str())?;
    let detail = format!(
        "catalog {} from {} matches known PE pressing: {} - {}",
        candidate.raw,
        candidate.source.label(),
        artist,
        title
    );

    Some(CatalogMatch {
        evidence: PreemphasisEvidence::CatalogExact,
        confidence: PreemphasisConfidence::Detected,
        catalog_number: candidate.raw.clone(),
        series_name: None,
        detail,
    })
}

fn match_series_catalog_candidate(candidate: &CatalogCandidate) -> Option<CatalogMatch> {
    for (pattern, series_name) in KNOWN_PE_SERIES.iter() {
        if pattern.is_match(candidate.normalized.as_str()) {
            let detail = format!(
                "catalog {} from {} matches PE series: {}",
                candidate.raw,
                candidate.source.label(),
                series_name
            );
            return Some(CatalogMatch {
                evidence: PreemphasisEvidence::CatalogSeries,
                confidence: PreemphasisConfidence::Possible,
                catalog_number: candidate.raw.clone(),
                series_name: Some(series_name.to_string()),
                detail,
            });
        }
    }

    None
}

// -- Known PE series (normalized regex patterns -> Possible) -----------------

lazy_static! {
    static ref KNOWN_PE_SERIES: Vec<(Regex, &'static str)> = vec![
        (
            Regex::new(r"^35DP[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 35DP"
        ),
        (
            Regex::new(r"^50DP[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 50DP (doubles)"
        ),
        (
            Regex::new(r"^32DP[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 32DP"
        ),
        (
            Regex::new(r"^35DC[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 35DC (classical)"
        ),
        (
            Regex::new(r"^56DC[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 56DC (classical doubles)"
        ),
        (
            Regex::new(r"^35DH[0-9]{1,3}$").unwrap(),
            "Japan CBS/Sony 35DH"
        ),
        (
            Regex::new(r"^358P[0-9]{1,3}$").unwrap(),
            "Japan Epic-Sony 35.8P"
        ),
        (
            Regex::new(r"^CP353[0-9]{3}$").unwrap(),
            "Japan Toshiba-EMI CP35-3xxx"
        ),
        (
            Regex::new(r"^CP325[0-9]{3}$").unwrap(),
            "Japan Chrysalis/Toshiba CP32-5xxx"
        ),
        (Regex::new(r"^38XB[0-9]+$").unwrap(), "Japan A&M 38XB"),
        (Regex::new(r"^CDV[0-9]{4,5}$").unwrap(), "UK Virgin CDV"),
        (
            Regex::new(r"^AGCD[0-9]{3,4}$").unwrap(),
            "American Gramaphone AGCD"
        ),
    ];
}

// ── Known PE exact matches (normalized catalog → (artist, title)) ──
//
// Source: https://www.studio-nibble.com/cd/index.php?title=Pre-emphasis_(release_list)
// Catalog numbers are normalized: uppercase, no spaces/dashes/dots.

lazy_static! {
    static ref KNOWN_PE_EXACT: HashMap<&'static str, (&'static str, &'static str)> = {
        let mut m = HashMap::new();

        // ── A ──
        m.insert("JRCD8011", ("A Flock of Seagulls", "A Flock of Seagulls"));
        m.insert("35DP025", ("Asia", "Asia"));
        m.insert("35DP25", ("Asia", "Asia"));

        // ── B ──
        m.insert("80001302", ("Barclay James Harvest", "Turn of the Tide"));
        m.insert("CP353016", ("The Beatles", "Abbey Road"));
        m.insert("358P005", ("Jeff Beck", "There and Back"));
        m.insert("358P5", ("Jeff Beck", "There and Back"));
        m.insert("35DP059", ("Leonard Bernstein", "West Side Story OST"));
        m.insert("CK40220", ("Big Audio Dynamite", "This Is Big Audio Dynamite"));
        m.insert("CD5284", ("The Blue Nile", "Hats"));
        m.insert("LXHCD2", ("The Blue Nile", "Hats"));
        m.insert("358P007", ("Boston", "Don't Look Back"));
        m.insert("358P7", ("Boston", "Don't Look Back"));
        m.insert("MCAD5809", ("T Bone Burnett", "T Bone Burnett"));

        // ── C ──
        m.insert("CP353018", ("Kim Carnes", "Mistaken Identity"));
        m.insert("CDV2286", ("China Crisis", "Working With Fire and Steel"));
        m.insert("ND71703", ("Clannad", "Legend"));
        m.insert("ROTACD1", ("Coil", "Horse Rotorvator"));

        // ── D ──
        m.insert("50DP237", ("Miles Davis", "Agharta"));
        m.insert("50DP239", ("Miles Davis", "Pangaea"));
        m.insert("35DP016", ("Miles Davis", "The Man with the Horn"));
        m.insert("35DP16", ("Miles Davis", "The Man with the Horn"));
        m.insert("CK36790", ("Miles Davis", "The Man with the Horn"));
        m.insert("35DP055", ("Miles Davis", "Star People"));
        m.insert("35DP55", ("Miles Davis", "Star People"));
        m.insert("35DP061", ("Miles Davis", "Porgy and Bess"));
        m.insert("35DP61", ("Miles Davis", "Porgy and Bess"));
        m.insert("CK08163", ("Miles Davis", "Kind of Blue"));
        m.insert("CK08271", ("Miles Davis", "Sketches of Spain"));
        m.insert("35DP064", ("Miles Davis", "Someday My Prince Will Come"));
        m.insert("35DP065", ("Miles Davis", "My Funny Valentine"));
        m.insert("35DP066", ("Miles Davis", "Four and More"));
        m.insert("35DP067", ("Miles Davis", "Miles in Tokyo"));
        m.insert("35DP068", ("Miles Davis", "Miles in Berlin"));
        m.insert("35DP069", ("Miles Davis", "E.S.P."));
        m.insert("35DP070", ("Miles Davis", "In a Silent Way"));
        m.insert("35DP170", ("Miles Davis", "Decoy"));
        m.insert("CDP7462422", ("Deep Purple", "Machine Head"));
        m.insert("35DP030", ("Al DiMeola", "Electric Rendezvous"));
        m.insert("35DP30", ("Al DiMeola", "Electric Rendezvous"));
        m.insert("9252642", ("Dire Straits", "Brothers In Arms"));

        // ── E ──
        m.insert("35DP024", ("Electric Light Orchestra", "Discovery"));
        m.insert("35DP24", ("Electric Light Orchestra", "Discovery"));
        m.insert("4500832", ("Electric Light Orchestra", "Discovery"));
        m.insert("35DP072", ("Electric Light Orchestra", "Secret Messages"));

        // ── F-G ──
        m.insert("BCD2501", ("Virgil Fox", "Plays the John Wanamaker Organ"));
        m.insert("SNEG312", ("Genesis", "Land of Confusion"));
        m.insert("35DP146", ("The Go-Go's", "Talk Show"));

        // ── H ──
        m.insert("CDV2208", ("Heaven 17", "Penthouse and Pavement"));
        m.insert("CDV2253", ("Heaven 17", "The Luxury Gap"));
        m.insert("35DH098", ("Hound Dog", "Dreamer"));
        m.insert("CDV2192", ("The Human League", "Dare"));
        m.insert("CD4892", ("The Human League", "Dare"));

        // ── J ──
        m.insert("ESCA5407", ("Michael Jackson", "Off The Wall"));
        m.insert("358P011", ("Michael Jackson", "Thriller"));
        m.insert("358P11", ("Michael Jackson", "Thriller"));
        m.insert("CDEPC85930", ("Michael Jackson", "Thriller"));
        m.insert("CDEPC86112", ("The Jacksons", "Triumph"));
        m.insert("EK36424", ("The Jacksons", "Triumph"));
        m.insert("EK35552", ("The Jacksons", "Destiny"));
        m.insert("CDP321082", ("Jethro Tull", "Minstrel in the Gallery"));
        m.insert("CCD1380", ("Jethro Tull", "The Broadsword and the Beast"));
        m.insert("610203", ("Jethro Tull", "Under Wraps"));
        m.insert("35DP001", ("Billy Joel", "52nd St."));
        m.insert("35DP1", ("Billy Joel", "52nd St."));
        m.insert("35DP002", ("Billy Joel", "The Stranger"));
        m.insert("35DP2", ("Billy Joel", "The Stranger"));
        m.insert("35DP018", ("Billy Joel", "Glass Houses"));
        m.insert("35DP18", ("Billy Joel", "Glass Houses"));
        m.insert("35DP019", ("Billy Joel", "Songs in the Attic"));
        m.insert("35DP19", ("Billy Joel", "Songs in the Attic"));
        m.insert("35DP034", ("Billy Joel", "The Nylon Curtain"));
        m.insert("35DP34", ("Billy Joel", "The Nylon Curtain"));
        m.insert("CDCBS80719", ("Billy Joel", "Piano Man"));
        m.insert("CK32544", ("Billy Joel", "Piano Man"));
        m.insert("24033522", ("Howard Jones", "Human's Lib"));
        m.insert("35DP006", ("Journey", "Escape"));
        m.insert("35DP6", ("Journey", "Escape"));
        m.insert("35DP045", ("Journey", "Frontiers"));
        m.insert("35DP45", ("Journey", "Frontiers"));

        // ── K-L ──
        m.insert("822769", ("Mark Knopfler", "Cal"));
        m.insert("358P027", ("Loverboy", "Keep It Up"));
        m.insert("358P27", ("Loverboy", "Keep It Up"));
        m.insert("FIENDCD99", ("Nick Lowe", "Pinker And Prouder Than Previous"));

        // ── M ──
        m.insert("AGCD355", ("Mannheim Steamroller", "Fresh Aire"));
        m.insert("AGCD359", ("Mannheim Steamroller", "Fresh Aire II"));
        m.insert("AGCD365", ("Mannheim Steamroller", "Fresh Aire III"));
        m.insert("AGCD370", ("Mannheim Steamroller", "Fresh Aire 4"));
        m.insert("AGCD385", ("Mannheim Steamroller", "Fresh Aire V"));
        m.insert("AGCD386", ("Mannheim Steamroller", "Fresh Aire VI"));
        m.insert("AGCD1984", ("Mannheim Steamroller", "Christmas"));
        m.insert("35DP033", ("Wynton Marsalis", "Wynton Marsalis"));
        m.insert("35DP33", ("Wynton Marsalis", "Wynton Marsalis"));
        m.insert("CP353001", ("Paul McCartney", "Tug of War"));
        m.insert("CDP746018", ("Paul McCartney", "Pipes of Peace"));
        m.insert("CDP746043", ("Paul McCartney", "Give My Regards to Broad Street"));
        m.insert("CK39613", ("Paul McCartney", "Give My Regards to Broad Street"));
        m.insert("CK37462", ("Paul McCartney", "Tug of War"));
        m.insert("358P015", ("Men at Work", "Business As Usual"));
        m.insert("358P15", ("Men at Work", "Business As Usual"));
        m.insert("358P016", ("Men at Work", "Cargo"));
        m.insert("358P16", ("Men at Work", "Cargo"));
        m.insert("CK38660", ("Men at Work", "Cargo"));

        // ── N-O ──
        m.insert("EK39294", ("Nena", "99 Luftballons"));
        m.insert("FACD100", ("New Order", "Low-life"));
        m.insert("VJD23001", ("Mike Oldfield", "Tubular Bells"));
        m.insert("CDV2001", ("Mike Oldfield", "Tubular Bells"));
        m.insert("CDID12", ("OMD", "Architecture & Morality"));

        // ── P ──
        m.insert("CDP746271", ("Pet Shop Boys", "Please"));
        m.insert("MCAD5486", ("Tom Petty", "Southern Accents"));
        m.insert("CP353017", ("Pink Floyd", "The Dark Side of the Moon"));
        m.insert("CDP746001", ("Pink Floyd", "The Dark Side of the Moon"));
        m.insert("35DP004", ("Pink Floyd", "Wish You Were Here"));
        m.insert("35DP4", ("Pink Floyd", "Wish You Were Here"));
        m.insert("CK33453", ("Pink Floyd", "Wish You Were Here"));
        m.insert("50DP361", ("Pink Floyd", "The Wall"));
        m.insert("35DP053", ("Pink Floyd", "The Final Cut"));
        m.insert("35DP53", ("Pink Floyd", "The Final Cut"));
        m.insert("CK38243", ("Pink Floyd", "The Final Cut"));
        m.insert("38XB2", ("The Police", "Synchronicity"));

        // ── R ──
        m.insert("358P004", ("REO Speedwagon", "Hi Infidelity"));
        m.insert("358P4", ("REO Speedwagon", "Hi Infidelity"));
        m.insert("MCD06059MD", ("Lionel Richie", "Can't Slow Down"));
        m.insert("CDV30050", ("Rita Mitsouko", "Rita Mitsouko"));

        // ── S ──
        m.insert("DIXCD40", ("Sandra", "The Long Play"));
        m.insert("35DP023", ("Santana", "Shango"));
        m.insert("35DP23", ("Santana", "Shango"));
        m.insert("35DP058", ("Santana", "Abraxas"));
        m.insert("35DP58", ("Santana", "Abraxas"));
        m.insert("CK39527", ("Santana", "Beyond Appearances"));
        m.insert("35DP003", ("Boz Scaggs", "Middle Man"));
        m.insert("35DP3", ("Boz Scaggs", "Middle Man"));
        m.insert("35DP011", ("Boz Scaggs", "Hits!"));
        m.insert("35DP11", ("Boz Scaggs", "Hits!"));
        m.insert("35DP020", ("Boz Scaggs", "Silk Degrees"));
        m.insert("35DP20", ("Boz Scaggs", "Silk Degrees"));
        m.insert("35DP013", ("Simon and Garfunkel", "The Simon and Garfunkel Collection"));
        m.insert("35DP13", ("Simon and Garfunkel", "The Simon and Garfunkel Collection"));
        m.insert("32JC129", ("The Smiths", "Hatful of Hollow"));
        m.insert("35DP021", ("Bruce Springsteen", "Born to Run"));
        m.insert("35DP21", ("Bruce Springsteen", "Born to Run"));
        m.insert("CDCBS80959", ("Bruce Springsteen", "Born to Run"));
        m.insert("32DP351", ("Bruce Springsteen", "Darkness on the Edge of Town"));
        m.insert("35DP007", ("Barbra Streisand", "Guilty"));
        m.insert("35DP7", ("Barbra Streisand", "Guilty"));
        m.insert("35DP161", ("Barbra Streisand", "Greatest Hits Volume 2"));
        m.insert("ZK38062", ("Survivor", "Eye of the Tiger"));

        // ── T ──
        m.insert("CDV2212", ("Tangerine Dream", "Exit"));
        m.insert("BCD2503", ("Michael Lee Thomas", "Voyager: Grand Tour Suite"));
        m.insert("35DP005", ("Toto", "Turn Back"));
        m.insert("35DP5", ("Toto", "Turn Back"));
        m.insert("35DP012", ("Toto", "IV"));
        m.insert("35DP12", ("Toto", "IV"));
        m.insert("4500882", ("Toto", "IV"));
        m.insert("35DP042", ("Toto", "Hydra"));
        m.insert("35DP42", ("Toto", "Hydra"));

        // ── U-V ──
        m.insert("CCD1296", ("Ultravox", "Vienna"));
        m.insert("8296632Y1", ("Vangelis", "Opera sauvage"));
        m.insert("8315032", ("Vangelis", "L'Apocalypse des Animaux"));

        // ── W ──
        m.insert("35DC117", ("Bruno Walter", "Bruckner: Symphony No.4"));
        m.insert("35DC114", ("Bruno Walter", "Bruckner: Symphony No.9"));
        m.insert("35DC086", ("Bruno Walter", "Brahms: Symphony No.2"));
        m.insert("35DC087", ("Bruno Walter", "Brahms: Symphony No.3"));
        m.insert("56DC131", ("Bruno Walter", "Mahler: Symphony No.2"));
        m.insert("35DP149", ("Roger Waters", "The Pros and Cons of Hitch Hiking"));
        m.insert("CDP746029", ("Roger Waters", "The Pros and Cons of Hitch Hiking"));
        m.insert("CK39290", ("Roger Waters", "The Pros and Cons of Hitch Hiking"));
        m.insert("50DP133", ("Weather Report", "8:30"));
        m.insert("VSD5353", ("John Williams", "The Empire Strikes Back"));
        m.insert("CDFA3101", ("Wings", "Wild Life"));

        // ── X ──
        m.insert("CDV2581", ("XTC", "Oranges and Lemons"));

        // ── Additional from user's library ──
        m.insert("35DP014", ("Simon & Garfunkel", "Bridge Over Troubled Water"));
        m.insert("35DP14", ("Simon & Garfunkel", "Bridge Over Troubled Water"));
        m.insert("358P002", ("Michael Jackson", "Off the Wall"));
        m.insert("358P2", ("Michael Jackson", "Off the Wall"));
        m.insert("CP325078", ("Heart", "Heart"));
        m.insert("CP325090", ("Michael Schenker Group", "The Michael Schenker Group"));
        m.insert("CP325091", ("Michael Schenker Group", "MSG"));
        m.insert("CP325092", ("Michael Schenker Group", "Assault Attack"));
        m.insert("CP325093", ("Michael Schenker Group", "Built to Destroy"));
        m.insert("CP325094", ("Michael Schenker Group", "Rock Will Never Die"));
        m.insert("CP325121", ("Michael Schenker Group", "One Night at Budokan"));
        m.insert("32PD17", ("Genesis", "Genesis"));
        m.insert("20VD1073", ("Genesis", "Land of Confusion"));
        m.insert("CSCS6077", ("Santana", "Shango"));
        m.insert("35DP131", ("Weather Report", "Heavy Weather"));
        m.insert("35DP008", ("Weather Report", "Night Passage"));
        m.insert("35DP8", ("Weather Report", "Night Passage"));

        m
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(raw: &str) -> CatalogCandidate {
        CatalogCandidate {
            raw: raw.to_string(),
            normalized: normalize_catalog(raw),
            source: CatalogSource::Tag,
        }
    }

    fn normalized_candidates(input: &str) -> Vec<String> {
        extract_catalog_candidates_from_text(input, CatalogSource::Folder)
            .into_iter()
            .map(|candidate| candidate.normalized)
            .collect()
    }

    #[test]
    fn test_normalize_catalog() {
        assert_eq!(normalize_catalog("35DP-25"), "35DP25");
        assert_eq!(normalize_catalog("35.8P-002"), "358P002");
        assert_eq!(normalize_catalog("CP32-5090"), "CP325090");
        assert_eq!(normalize_catalog("CDV 2192"), "CDV2192");
        assert_eq!(normalize_catalog("  ck 08163  "), "CK08163");
    }

    #[test]
    fn test_exact_match_lookup() {
        assert!(KNOWN_PE_EXACT.contains_key("35DP025"));
        assert!(KNOWN_PE_EXACT.contains_key("35DP25"));
        assert!(KNOWN_PE_EXACT.contains_key("CDV2192"));
        assert!(KNOWN_PE_EXACT.contains_key("CP353016"));
        assert!(!KNOWN_PE_EXACT.contains_key("NONEXISTENT"));
    }

    #[test]
    fn test_series_match_uses_normalized_catalogs() {
        let cases = [
            "35DP-999",
            "35DP 999",
            "35.8P-7",
            "358P-7",
            "CP35-3016",
            "CDV 2192",
            "AGCD-355",
        ];

        for raw in cases {
            let normalized = normalize_catalog(raw);
            let matched = KNOWN_PE_SERIES
                .iter()
                .any(|(pattern, _)| pattern.is_match(normalized.as_str()));
            assert!(matched, "{raw} should match after normalization");
        }

        let rejected = ["CP35-2016", "MFSL", "35DP-1234"];
        for raw in rejected {
            let normalized = normalize_catalog(raw);
            let matched = KNOWN_PE_SERIES
                .iter()
                .any(|(pattern, _)| pattern.is_match(normalized.as_str()));
            assert!(!matched, "{raw} should not match");
        }
    }

    #[test]
    fn test_catalog_extracts_expected_candidates() {
        let cases = [
            ("Japan CBS-Sony 35DP-25", vec!["35DP25"]),
            ("Japan Epic-Sony 35.8P-7", vec!["358P7"]),
            ("Japan CP32-5090", vec!["CP325090"]),
            ("Japan A&M Records 38XB-2", vec!["38XB2"]),
            ("UK Virgin CDV 2192", vec!["CDV2192"]),
            ("Columbia CK 08163", vec!["CK08163"]),
            ("Pet Shop Boys CDP 746271", vec!["CDP746271"]),
        ];

        for (input, expected) in cases {
            assert_eq!(normalized_candidates(input), expected, "failed for {input}");
        }
    }

    #[test]
    fn test_catalog_extraction_rejects_embedded_or_extended_tokens() {
        let rejected = [
            "foo35DP-25",
            "35DP-25bar",
            "Japan CBS-Sony 35DP-1234",
            "Japan CBS-Sony 35DP-123-4",
            "Epic 35.8P-1234",
            "Columbia CK081634",
        ];

        for input in rejected {
            assert!(
                normalized_candidates(input).is_empty(),
                "{input} should not produce a candidate"
            );
        }
    }

    #[test]
    fn test_tag_text_can_contain_extra_catalog_text() {
        let candidates = tag_value_candidates("35DP-25 (DIDP 50001)", CatalogSource::Tag);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].normalized, "35DP25");

        let matched = match_catalog_candidate(candidates.into_iter().next().unwrap());
        assert!(matched.is_some());
        assert!(matches!(
            matched.unwrap().confidence,
            PreemphasisConfidence::Detected
        ));
    }

    #[test]
    fn test_exact_match_precedes_series_match() {
        let matched = match_catalog_candidate(candidate("CP35-3016")).unwrap();
        assert!(matches!(
            matched.confidence,
            PreemphasisConfidence::Detected
        ));
        assert!(matches!(
            matched.evidence,
            PreemphasisEvidence::CatalogExact
        ));
    }

    #[test]
    fn test_series_fallback_still_works() {
        let matched = match_catalog_candidate(candidate("35DP-999")).unwrap();
        assert!(matches!(
            matched.confidence,
            PreemphasisConfidence::Possible
        ));
        assert!(matches!(
            matched.evidence,
            PreemphasisEvidence::CatalogSeries
        ));
    }

    #[test]
    fn test_numeric_catalogs_must_be_known_exact_values() {
        assert_eq!(normalized_candidates("Dire Straits 9252642"), vec!["9252642"]);
        assert!(normalized_candidates("random remaster 1234567").is_empty());
    }

    #[test]
    fn test_multiple_candidates_are_preserved_in_order() {
        let candidates = normalized_candidates("Miles Davis CK08163 and 35DP-61");
        assert_eq!(candidates, vec!["CK08163", "35DP61"]);
    }

    #[test]
    fn test_folder_extracts_numeric_letter_digit_exact_catalog() {
        assert_eq!(
            normalized_candidates("Vangelis 8296632Y1"),
            vec!["8296632Y1"]
        );
    }

    #[test]
    fn test_whitespace_digit_continuation_is_rejected() {
        assert!(normalized_candidates("Japan CBS-Sony 35DP 123 4").is_empty());
    }

    #[test]
    fn test_punctuation_letter_suffix_is_rejected() {
        assert!(normalized_candidates("UK Virgin CDV-2192-A").is_empty());
        assert!(normalized_candidates("CBS 35DP-25-A").is_empty());
    }

    #[test]
    fn test_grouped_folder_catalog_formats_are_normalized() {
        assert_eq!(
            normalized_candidates("Deep Purple CDP 7 46242 2"),
            vec!["CDP7462422"]
        );
        assert_eq!(
            normalized_candidates("Enya MCD 06059 MD"),
            vec!["MCD06059MD"]
        );
        assert_eq!(normalized_candidates("35 8P-11"), vec!["358P11"]);
        assert_eq!(normalized_candidates("FIEND-CD-99"), vec!["FIENDCD99"]);
    }

    #[test]
    fn test_catalog_before_year_is_accepted() {
        assert_eq!(
            normalized_candidates("Asia 35DP-25 1982 Japan"),
            vec!["35DP25"]
        );
        assert_eq!(
            normalized_candidates("UK Virgin CDV 2192 1981"),
            vec!["CDV2192"]
        );
        assert_eq!(
            normalized_candidates("Miles Davis CK08163 1986 Columbia"),
            vec!["CK08163"]
        );
        assert_eq!(
            normalized_candidates("Pet Shop Boys CDP 746271 1986"),
            vec!["CDP746271"]
        );
    }

    #[test]
    fn test_non_year_whitespace_digit_continuation_is_rejected() {
        assert!(normalized_candidates("Japan CBS-Sony 35DP 123 4").is_empty());
        assert!(normalized_candidates("UK Virgin CDV 2192 1").is_empty());
        assert!(normalized_candidates("Pet Shop Boys CDP 746271 1234").is_empty());
    }

    #[test]
    fn test_period_after_catalog_can_be_sentence_punctuation() {
        assert_eq!(
            normalized_candidates("UK Virgin CDV-2192. Japan first pressing"),
            vec!["CDV2192"]
        );
        assert_eq!(
            normalized_candidates("CBS 35DP-25. Japan for Europe"),
            vec!["35DP25"]
        );
    }

    #[test]
    fn test_immediate_suffixes_after_catalog_are_rejected() {
        assert!(normalized_candidates("UK Virgin CDV-2192.1").is_empty());
        assert!(normalized_candidates("UK Virgin CDV-2192-A").is_empty());
        assert!(normalized_candidates("CBS 35DP-25-A").is_empty());
    }

    #[test]
    fn test_space_separated_short_suffixes_are_rejected() {
        assert!(normalized_candidates("CBS 35DP-25 A").is_empty());
        assert!(normalized_candidates("CBS 35DP-25 A1").is_empty());
        assert!(normalized_candidates("CBS 35DP-25 1A").is_empty());
    }

    #[test]
    fn test_parenthesized_context_after_catalog_is_accepted() {
        assert_eq!(
            normalized_candidates("CBS 35DP-25 (Japan first pressing)"),
            vec!["35DP25"]
        );
        assert_eq!(
            normalized_candidates("CBS 35DP-25 (1982 Japan first pressing)"),
            vec!["35DP25"]
        );
    }

    #[test]
    fn test_best_match_prefers_exact_over_earlier_series_match() {
        let candidates = vec![
            CatalogCandidate {
                raw: "35DP-999".to_string(),
                normalized: "35DP999".to_string(),
                source: CatalogSource::Tag,
            },
            CatalogCandidate {
                raw: "CP35-3016".to_string(),
                normalized: "CP353016".to_string(),
                source: CatalogSource::Folder,
            },
        ];

        let matched = best_catalog_match(&candidates).unwrap();
        assert!(matches!(
            matched.confidence,
            PreemphasisConfidence::Detected
        ));
        assert!(matches!(
            matched.evidence,
            PreemphasisEvidence::CatalogExact
        ));
        assert_eq!(matched.catalog_number, "CP35-3016");
    }
}
