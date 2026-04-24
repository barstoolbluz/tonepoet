//! Catalog-number-based pre-emphasis detection.
//!
//! Matches catalog numbers from audio tags or folder names against:
//! 1. A database of exact known-PE catalog numbers (→ Detected)
//! 2. Regex patterns for known PE series (→ Possible)
//!
//! Source: https://www.studio-nibble.com/cd/index.php?title=Pre-emphasis_(release_list)

use std::collections::HashMap;
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

// ── Catalog extraction ─────────────────────────────────────────────

lazy_static! {
    /// Regex to extract catalog numbers from folder names.
    /// Matches Japanese and international catalog patterns.
    static ref CATALOG_EXTRACT: Regex = Regex::new(concat!(
        r"(?i)(",
        r"35DP[-\s·]?\d{1,3}",          // CBS/Sony Japan 35DP
        r"|50DP[-\s·]?\d{1,3}",          // CBS/Sony Japan 50DP (doubles)
        r"|32DP[-\s·]?\d{1,3}",          // CBS/Sony Japan 32DP
        r"|35DC[-\s·]?\d{1,3}",          // CBS/Sony Japan classical
        r"|56DC[-\s·]?\d{1,3}",          // CBS/Sony Japan classical doubles
        r"|35DH[-\s·]?\d{1,3}",          // CBS/Sony Japan 35DH
        r"|35[·.]?8P[-\s·]?\d{1,3}",    // Epic-Sony Japan 35.8P
        r"|CP35[-\s·]?\d{4}",            // Toshiba-EMI Japan
        r"|CP32[-\s·]?\d{4}",            // Chrysalis/Toshiba Japan
        r"|38XB[-\s·]?\d+",              // A&M Japan
        r"|32PD[-\s·]?\d+",              // Philips Japan
        r"|20VD[-\s·]?\d+",              // Victor Japan
        r"|32JC[-\s·]?\d+",              // Japan Record
        r"|CDV[-\s]?\d{4,5}",            // UK Virgin CDV
        r"|CSCS[-\s·]?\d{4}",            // CBS/Sony Japan reissue
        r"|SRCS[-\s·]?\d{4}",            // Sony Japan reissue
        r"|ESCA[-\s·]?\d{4}",            // Epic/Sony Japan
        r"|AGCD[-\s·]?\d{3,4}",          // American Gramaphone
        r"|CK[-\s]?\d{5}",               // US Columbia CK
        r"|EK[-\s]?\d{5}",               // US Epic EK
        r"|ZK[-\s]?\d{5}",               // US Columbia ZK
        r"|Facd[-\s]?\d+",               // Factory Records
        r"|FIEND\s?CD[-\s]?\d+",         // Demon Records
        r")"
    )).unwrap();
}

/// Normalize a catalog number for database lookup.
/// Strips spaces, dashes, dots, and converts to uppercase.
fn normalize_catalog(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

/// Extract a catalog number from the parent folder name.
fn extract_catalog_from_folder(audio_path: &Path) -> Option<String> {
    let parent = audio_path.parent()?;
    let folder_name = parent.file_name()?.to_str()?;
    CATALOG_EXTRACT.find(folder_name).map(|m| m.as_str().to_string())
}

/// Extract a catalog number from the audio file's tags.
fn extract_catalog_from_tags(audio_path: &Path) -> Option<String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let tagged = lofty::read_from_path(audio_path).ok()?;
    for tag in tagged.tags() {
        // Try standard CatalogNumber key.
        if let Some(val) = tag.get_string(&ItemKey::CatalogNumber) {
            let v = val.trim();
            if !v.is_empty() { return Some(v.to_string()); }
        }
        // Try freeform CATALOGNUMBER.
        if let Some(val) = tag.get_string(&ItemKey::Unknown("CATALOGNUMBER".to_string())) {
            let v = val.trim();
            if !v.is_empty() { return Some(v.to_string()); }
        }
    }
    None
}

// ── Public API ─────────────────────────────────────────────────────

/// Check if the audio file's catalog number matches a known PE pressing.
///
/// Returns:
/// - `CatalogExact` + `Detected` for exact catalog+title matches
/// - `CatalogSeries` + `Possible` for catalog numbers in known PE series
/// - `None` if no catalog match
pub fn check_catalog_evidence(audio_path: &Path) -> Option<CatalogMatch> {
    // Try tag first, then folder name.
    let raw_catalog = extract_catalog_from_tags(audio_path)
        .or_else(|| extract_catalog_from_folder(audio_path))?;

    let normalized = normalize_catalog(&raw_catalog);
    if normalized.is_empty() { return None; }

    // Check exact match.
    if let Some(&(artist, title)) = KNOWN_PE_EXACT.get(normalized.as_str()) {
        let detail = format!("catalog {} matches known PE pressing: {} - {}", &raw_catalog, artist, title);
        return Some(CatalogMatch {
            evidence: PreemphasisEvidence::CatalogExact,
            confidence: PreemphasisConfidence::Detected,
            catalog_number: raw_catalog,
            series_name: None,
            detail,
        });
    }

    // Check series match.
    for (pattern, series_name) in KNOWN_PE_SERIES.iter() {
        if pattern.is_match(&raw_catalog) {
            let detail = format!("catalog {} matches PE series: {}", &raw_catalog, series_name);
            return Some(CatalogMatch {
                evidence: PreemphasisEvidence::CatalogSeries,
                confidence: PreemphasisConfidence::Possible,
                catalog_number: raw_catalog,
                series_name: Some(series_name.to_string()),
                detail,
            });
        }
    }

    None
}

// ── Known PE series (regex patterns → Possible) ────────────────────

lazy_static! {
    static ref KNOWN_PE_SERIES: Vec<(Regex, &'static str)> = vec![
        (Regex::new(r"(?i)^35DP[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 35DP"),
        (Regex::new(r"(?i)^50DP[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 50DP (doubles)"),
        (Regex::new(r"(?i)^32DP[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 32DP"),
        (Regex::new(r"(?i)^35DC[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 35DC (classical)"),
        (Regex::new(r"(?i)^56DC[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 56DC (classical doubles)"),
        (Regex::new(r"(?i)^35DH[-\s·]?\d{1,3}$").unwrap(), "Japan CBS/Sony 35DH"),
        (Regex::new(r"(?i)^35[·.]?8P[-\s·]?\d{1,3}$").unwrap(), "Japan Epic-Sony 35.8P"),
        (Regex::new(r"(?i)^CP35[-\s·]?3\d{3}$").unwrap(), "Japan Toshiba-EMI CP35-3xxx"),
        (Regex::new(r"(?i)^CP32[-\s·]?5\d{3}$").unwrap(), "Japan Chrysalis/Toshiba CP32-5xxx"),
        (Regex::new(r"(?i)^38XB[-\s·]?\d+$").unwrap(), "Japan A&M 38XB"),
        (Regex::new(r"(?i)^CDV[-\s]?\d{4,5}$").unwrap(), "UK Virgin CDV"),
        (Regex::new(r"(?i)^AGCD[-\s·]?\d{3,4}$").unwrap(), "American Gramaphone AGCD"),
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

    #[test]
    fn test_normalize_catalog() {
        assert_eq!(normalize_catalog("35DP-25"), "35DP25");
        assert_eq!(normalize_catalog("35.8P-002"), "358P002");
        assert_eq!(normalize_catalog("CP32-5090"), "CP325090");
        assert_eq!(normalize_catalog("CDV 2192"), "CDV2192");
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
    fn test_series_match() {
        // 35DP-999 is not in the exact database but matches the 35DP series.
        let raw = "35DP-999";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(matched, "35DP-999 should match 35DP series");

        // 35.8P with unicode dot should match.
        let raw = "35·8P-7";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(matched, "35·8P-7 should match 35.8P series");

        // 35.8P with ASCII dot should match.
        let raw = "35.8P-7";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(matched, "35.8P-7 should match 35.8P series");

        // CP35-3016 should match (3xxx prefix).
        let raw = "CP35-3016";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(matched, "CP35-3016 should match CP35-3xxx series");

        // CP35-2016 should NOT match (not 3xxx).
        let raw = "CP35-2016";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(!matched, "CP35-2016 should not match CP35-3xxx series");

        // CDV2192 (no space) should match.
        let raw = "CDV2192";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(matched, "CDV2192 should match UK Virgin CDV series");

        // MFSL should not match any series.
        let raw = "MFSL";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(!matched, "MFSL should not match any PE series");

        // 35DP-1234 (4 digits) should NOT match (series allows 1-3 digits).
        let raw = "35DP-1234";
        let matched = KNOWN_PE_SERIES.iter().any(|(pat, _)| pat.is_match(raw));
        assert!(!matched, "35DP-1234 should not match (too many digits)");
    }

    #[test]
    fn test_catalog_extract_regex() {
        let cases = vec![
            ("Japan  CBS-Sony 35DP-25", Some("35DP-25")),
            ("Japan  Epic-Sony 35.8P-7", Some("35.8P-7")),
            ("Japan  CP32-5090", Some("CP32-5090")),
            ("MFSL UltraDisc UHR", None),
            ("Japan  A&M Records 38XB-2", Some("38XB-2")),
        ];
        for (input, expected) in cases {
            let found = CATALOG_EXTRACT.find(input).map(|m| m.as_str());
            assert_eq!(found, expected, "Failed for input: {}", input);
        }
    }
}
