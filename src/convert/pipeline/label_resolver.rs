//! Label, country, pressing, and artist normalization for naming templates.
//!
//! `LabelResolver` implementations fill `%LABEL%`, `%COUNTRY%`, and
//! `%PRESSING%` values through `AlbumMetadata.extra`. Existing tag-sourced
//! values keep priority because `enrich_with_label_info` inserts only missing
//! keys.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use super::types::AlbumMetadata;

const HEXLOAD_LABELS_REFERENCE: &str = include_str!("../../../docs/hexload_labels_reference.rs");
const CANONICAL_ARTISTS_REFERENCE: &str =
    include_str!("../../../docs/canonical_artists_reference.txt");

const CATALOG_COUNTRY_MAPPINGS: &[(&str, &str)] = &[
    ("AMCE", "Japan"),
    ("AMCY", "Japan"),
    ("BVCP", "Japan"),
    ("CDSOL", "Japan"),
    ("KICJ", "Japan"),
    ("MVCR", "Japan"),
    ("POCM", "Japan"),
    ("SICP", "Japan"),
    ("SRCS", "Japan"),
    ("SRGS", "Japan"),
    ("TECW", "Japan"),
    ("TOCJ", "Japan"),
    ("TYCJ", "Japan"),
    ("UCCI", "Japan"),
    ("UCCJ", "Japan"),
    ("UCCQ", "Japan"),
    ("UICY", "Japan"),
    ("VICJ", "Japan"),
    ("VICP", "Japan"),
    ("VJCP", "Japan"),
    ("VRCL", "Japan"),
    ("WPCR", "Japan"),
    ("SME", "Japan"),
    ("TOCP", "Japan"),
    ("MHCP", "Japan"),
    ("PHCR", "Japan"),
    ("VDJ", "Japan"),
    ("BVCM", "Japan"),
    ("UCCU", "Japan"),
    ("VACK", "Japan"),
    ("PCD", "Japan"),
    ("AIRAC", "Japan"),
    ("SICJ", "Japan"),
    ("UCCO", "Japan"),
    ("VDP", "Japan"),
    ("TODP", "Japan"),
    ("WPCP", "Japan"),
    ("UCCV", "Japan"),
    ("BVCJ", "Japan"),
    ("VJD", "Japan"),
    ("ESCA", "Japan"),
    ("CSCS", "Japan"),
    ("EICP", "Japan"),
    ("PPD", "Japan"),
    ("MVCZ", "Japan"),
    ("BVCA", "Japan"),
    ("TECP", "Japan"),
    ("WMC", "Japan"),
    ("POCD", "Japan"),
    ("POCJ", "Japan"),
    ("MVCG", "Japan"),
    ("VHCD", "Japan"),
    ("UICS", "Japan"),
    ("TECI", "Japan"),
    ("PCCY", "Japan"),
    ("GQBS", "Japan"),
    ("COCB", "Japan"),
    ("UCCE", "Japan"),
    ("MVCJ", "Japan"),
    ("WQCR", "Japan"),
    ("VSCD", "Japan"),
    ("UCCM", "Japan"),
    ("POCP", "Japan"),
    ("MYCJ", "Japan"),
    ("AVCD", "Japan"),
    ("KICP", "Japan"),
    ("VICL", "Japan"),
    ("UPCY", "Japan"),
    ("UCGO", "Japan"),
    ("TKCV", "Japan"),
    ("SHOUT", "Japan"),
    ("PPDM", "Japan"),
    ("PHDR", "Japan"),
    ("MVCI", "Japan"),
    ("MVCF", "Japan"),
    ("DIW", "Japan"),
    ("COCY", "Japan"),
    ("BSCP", "Japan"),
    ("BRJ", "Japan"),
    ("ALT", "Japan"),
];

const LABEL_COUNTRY_MAPPINGS: &[(&str, &str)] = &[
    ("Analogue Productions", "US"),
    ("Audio Fidelity", "US"),
    ("CBS-Sony", "Japan"),
    ("CBS Sony", "Japan"),
    ("Esoteric", "Japan"),
    ("DCC", "US"),
    ("MFSL", "US"),
    ("Nautilus", "US"),
    ("Nautilus SuperDisc", "US"),
    ("SHM", "Japan"),
    ("Toshiba", "Japan"),
    ("Toshiba-EMI", "Japan"),
    ("Toshiba EMI", "Japan"),
    ("King Records", "Japan"),
    ("Warner Pioneer", "Japan"),
    ("Epic Sony", "Japan"),
    ("Epic/Sony", "Japan"),
    ("CBS/Sony", "Japan"),
    ("Toshiba Pro-Use", "Japan"),
];

const CANONICAL_LABEL_VARIANTS: &[(&str, &[&str])] = &[
    ("Blue Note", &["Blue Note", "BlueNote", "Bluenote", "UMe"]),
    (
        "Blue Note Classic",
        &["Blue Note Classic", "Bluenote Classic", "BN Classic"],
    ),
    (
        "Blue Note Tone Poet",
        &["Blue Note Tone Poet", "Tone Poet", "TonePoet", "Tonepoet"],
    ),
    (
        "CBS-Sony",
        &[
            "CBS-Sony",
            "CBS/Sony",
            "CBS Sony",
            "CBSSony",
            "CBS_Sony",
            "CBS.Sony",
            "CBS - Sony",
        ],
    ),
    (
        "DCC",
        &["DCC", "DCC Compact Classics", "Digital Compact Classics"],
    ),
    (
        "MFSL",
        &[
            "MFSL",
            "MoFi",
            "Mofi",
            "Mobile Fidelity",
            "Mobile Fidelity Sound Lab",
            "UDCD",
        ],
    ),
    ("MFSL UltraDisc UHR", &["MFSL UltraDisc UHR"]),
    (
        "Nautilus SuperDisc",
        &[
            "Nautilus",
            "Nautilus Recordings",
            "Nautilus Super Disc",
            "Nautilus SuperDisc",
        ],
    ),
    ("Pure Pleasure", &["Pure Pleasure", "Pure Pleasure Records"]),
    (
        "Warner",
        &[
            "Warner",
            "Warner Bros",
            "Warner Bros.",
            "Warner Bros. Records",
            "Warner Brothers Records",
            "Warner Records",
        ],
    ),
    (
        "Analogue Productions",
        &[
            "Analogue Productions",
            "Analog Productions",
            "Acoustic Sounds",
            "AP",
        ],
    ),
    ("Audio Fidelity", &["Audio Fidelity"]),
    ("Analog Spark", &["Analog Spark", "Analogue Spark"]),
    ("Esoteric", &["Esoteric"]),
    ("XRCD", &["XRCD"]),
    ("XRCD2", &["XRCD2"]),
    ("XRCD24", &["XRCD24"]),
    ("SHM", &["SHM", "SHM-CD", "SHM CD"]),
    ("Toshiba", &["Toshiba"]),
    ("Toshiba-EMI", &["Toshiba-EMI", "Toshiba EMI"]),
    (
        "Toshiba Pro-Use",
        &["Toshiba Pro-Use", "Toshiba Pro Use", "Pro-Use", "Pro Use"],
    ),
    ("King Records", &["King Records", "King"]),
    ("Warner Pioneer", &["Warner Pioneer", "Warner-Pioneer"]),
    ("Epic Sony", &["Epic Sony", "Epic/Sony", "Epic-Sony"]),
    ("Harvest", &["Harvest"]),
    ("Vertigo", &["Vertigo"]),
    ("Island", &["Island"]),
    ("Track", &["Track"]),
    ("Decca", &["Decca"]),
    ("Columbia", &["Columbia"]),
    ("Parlophone", &["Parlophone"]),
    ("Apple", &["Apple"]),
    ("Atlantic", &["Atlantic"]),
    ("Factory", &["Factory"]),
    ("4AD", &["4AD"]),
    ("Creation", &["Creation"]),
    ("Rough Trade", &["Rough Trade"]),
    ("Abbey Road", &["Abbey Road"]),
    ("Townhouse", &["Townhouse"]),
    ("Odeon", &["Odeon"]),
    ("Charisma", &["Charisma"]),
    ("Mastersound", &["Mastersound", "Master Sound"]),
    ("Polydor", &["Polydor"]),
    ("Denon", &["Denon"]),
    ("A&M", &["A&M", "A and M", "A M"]),
    ("Elektra", &["Elektra"]),
    ("London", &["London"]),
    ("Philips", &["Philips"]),
    ("Teldec", &["Teldec"]),
    ("Hörzu", &["Hörzu", "Hoerzu", "Horzu"]),
    (
        "Music Matters",
        &["Music Matters", "MM", "MM33", "MM 33", "MM SRX"],
    ),
    ("Classic Records", &["Classic Records", "Classic"]),
    ("Speakers Corner", &["Speakers Corner"]),
    ("ORG Music", &["ORG", "ORG Music"]),
    ("Impex Records", &["Impex", "Impex Records"]),
    (
        "Intervention Records",
        &["Intervention", "Intervention Records"],
    ),
    ("Friday Music", &["Friday Music", "FRM", "FRMKG", "FRM KG"]),
    ("Music On Vinyl", &["Music On Vinyl", "MOV"]),
    (
        "High Roller Records",
        &["High Roller", "High Roller Records"],
    ),
];

const CANONICAL_COUNTRY_VARIANTS: &[(&str, &[&str])] = &[
    ("AUS", &["Australia", "AUS", "Australian"]),
    ("CA", &["Canada", "CA", "CDN", "Canadian"]),
    ("DE", &["German", "DE", "Germany"]),
    ("EU", &["Europe", "EU", "European"]),
    ("FR", &["French", "FR", "France"]),
    ("IT", &["Italy", "IT", "Italian"]),
    ("Japan", &["JPN", "JP", "Japanese", "Jap", "Jpn", "Japan"]),
    ("NL", &["Dutch", "NL", "Netherlands"]),
    (
        "UK",
        &["UK", "United Kingdom", "England", "Great Britain", "U.K."],
    ),
    (
        "US",
        &["US", "USA", "U.S.", "U.S.A.", "United States", "American"],
    ),
    (
        "West German",
        &["W. German", "W. Germany", "West Germany", "West German"],
    ),
];

const DIGITAL_MARKERS: &[(&str, &str)] = &[
    ("WEB", "DD"),
    ("WEB-DL", "DD"),
    ("HDTracks", "HDTracks"),
    ("Qobuz", "Qobuz"),
    ("eOnkyo", "eOnkyo"),
];

const PREMIUM_LABELS: &[&str] = &[
    "MFSL",
    "DCC",
    "Analogue Productions",
    "Audio Fidelity",
    "XRCD",
    "XRCD2",
    "XRCD24",
    "Analog Spark",
    "Esoteric",
];

/// Resolved label/pressing information from a `LabelResolver`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedLabel {
    pub label: Option<String>,
    pub country: Option<String>,
    pub pressing: Option<String>,
}

/// Trait for resolving label, country, and pressing information from
/// album metadata and container path.
pub trait LabelResolver: Send + Sync {
    fn resolve(&self, metadata: &AlbumMetadata, container: &Path) -> Option<ResolvedLabel>;
}

/// Stub resolver that always returns `None`.
pub struct StubLabelResolver;

impl LabelResolver for StubLabelResolver {
    fn resolve(&self, _metadata: &AlbumMetadata, _container: &Path) -> Option<ResolvedLabel> {
        None
    }
}

#[derive(Debug, Clone)]
struct Alias {
    normalized: String,
    canonical: String,
    score: usize,
}

#[derive(Debug, Clone)]
struct HexloadMapping {
    key: String,
    normalized_key: String,
    value: String,
    score: usize,
}

/// Dictionary-backed implementation for catalog-prefix, label, country, and
/// pressing resolution.
#[derive(Debug, Clone)]
pub struct DictionaryLabelResolver {
    catalog_countries: HashMap<String, String>,
    label_countries: HashMap<String, String>,
    exact_label_aliases: HashMap<String, String>,
    exact_country_aliases: HashMap<String, String>,
    label_aliases: Vec<Alias>,
    country_aliases: Vec<Alias>,
    hexload_mappings: Vec<HexloadMapping>,
}

impl Default for DictionaryLabelResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DictionaryLabelResolver {
    pub fn new() -> Self {
        let catalog_countries = CATALOG_COUNTRY_MAPPINGS
            .iter()
            .map(|(prefix, country)| (prefix.to_ascii_uppercase(), (*country).to_string()))
            .collect();

        let mut exact_label_aliases = HashMap::new();
        let mut label_aliases = Vec::new();
        for (canonical, variants) in CANONICAL_LABEL_VARIANTS {
            for variant in *variants {
                let normalized = normalize_for_match(variant);
                if normalized.is_empty() {
                    continue;
                }
                exact_label_aliases.insert(normalized.clone(), (*canonical).to_string());
                label_aliases.push(Alias {
                    score: normalized.len(),
                    normalized,
                    canonical: (*canonical).to_string(),
                });
            }
        }
        sort_aliases_longest_first(&mut label_aliases);

        let mut exact_country_aliases = HashMap::new();
        let mut country_aliases = Vec::new();
        for (canonical, variants) in CANONICAL_COUNTRY_VARIANTS {
            for variant in *variants {
                let normalized = normalize_for_match(variant);
                if normalized.is_empty() {
                    continue;
                }
                exact_country_aliases.insert(normalized.clone(), (*canonical).to_string());
                country_aliases.push(Alias {
                    score: normalized.len(),
                    normalized,
                    canonical: (*canonical).to_string(),
                });
            }
        }
        sort_aliases_longest_first(&mut country_aliases);

        let label_countries = LABEL_COUNTRY_MAPPINGS
            .iter()
            .map(|(label, country)| {
                let canonical_label = exact_label_aliases
                    .get(&normalize_for_match(label))
                    .cloned()
                    .unwrap_or_else(|| (*label).to_string());
                let canonical_country = exact_country_aliases
                    .get(&normalize_for_match(country))
                    .cloned()
                    .unwrap_or_else(|| (*country).to_string());
                (canonical_label, canonical_country)
            })
            .collect();

        let mut hexload_mappings = parse_hexload_mappings();
        hexload_mappings.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.key.cmp(&b.key)));

        Self {
            catalog_countries,
            label_countries,
            exact_label_aliases,
            exact_country_aliases,
            label_aliases,
            country_aliases,
            hexload_mappings,
        }
    }

    fn resolve_catalog_country(&self, metadata: &AlbumMetadata) -> Option<String> {
        catalog_value(&metadata.extra)
            .and_then(extract_catalog_prefix)
            .and_then(|prefix| self.catalog_countries.get(&prefix).cloned())
    }

    fn normalize_label(&self, label: &str) -> Option<String> {
        let normalized = normalize_for_match(label);
        if normalized.is_empty() {
            return None;
        }
        self.exact_label_aliases
            .get(&normalized)
            .cloned()
            .or_else(|| Some(label.trim().to_string()).filter(|value| !value.is_empty()))
    }

    fn normalize_country(&self, country: &str) -> Option<String> {
        let normalized = normalize_for_match(country);
        if normalized.is_empty() {
            return None;
        }
        self.exact_country_aliases
            .get(&normalized)
            .cloned()
            .or_else(|| Some(country.trim().to_string()).filter(|value| !value.is_empty()))
    }

    fn find_label_in_text(&self, text: &str) -> Option<String> {
        let normalized_text = normalize_for_match(text);
        self.label_aliases
            .iter()
            .find(|alias| contains_phrase(&normalized_text, &alias.normalized))
            .map(|alias| alias.canonical.clone())
    }

    fn find_country_in_text(&self, text: &str) -> Option<String> {
        let normalized_text = normalize_for_match(text);
        self.country_aliases
            .iter()
            .find(|alias| contains_phrase(&normalized_text, &alias.normalized))
            .map(|alias| alias.canonical.clone())
    }

    fn find_country_in_container_text(&self, text: &str) -> Option<String> {
        let normalized_text = normalize_for_match(text);
        self.country_aliases
            .iter()
            .find(|alias| contains_country_alias_in_container(text, &normalized_text, alias))
            .map(|alias| alias.canonical.clone())
    }

    fn find_hexload_mapping<'a>(&'a self, text: &str) -> Option<&'a HexloadMapping> {
        let normalized_text = normalize_for_match(text);
        self.hexload_mappings
            .iter()
            .find(|mapping| contains_phrase(&normalized_text, &mapping.normalized_key))
    }

    fn country_from_label(&self, label: Option<&str>) -> Option<String> {
        label.and_then(|value| self.label_countries.get(value).cloned())
    }

    fn country_from_hexload(&self, mapping: &HexloadMapping) -> Option<String> {
        self.find_country_in_text(&mapping.key)
            .or_else(|| self.find_country_in_text(&mapping.value))
    }

    fn label_from_hexload(&self, mapping: &HexloadMapping) -> Option<String> {
        self.find_label_in_text(&mapping.key)
            .or_else(|| self.find_label_in_text(&mapping.value))
    }
}

impl LabelResolver for DictionaryLabelResolver {
    fn resolve(&self, metadata: &AlbumMetadata, container: &Path) -> Option<ResolvedLabel> {
        let container_name = container
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let container_text = container_name.to_string();

        if let Some(label) = digital_download_label(metadata, &container_text) {
            return Some(ResolvedLabel {
                label: Some(label),
                country: None,
                pressing: None,
            });
        }

        let catalog_country = self.resolve_catalog_country(metadata);
        let hexload_match = self.find_hexload_mapping(&container_text);

        let tag_label = nonempty_extra(&metadata.extra, "label");
        let mut label = tag_label
            .and_then(|value| self.normalize_label(value))
            .or_else(|| self.find_label_in_text(&container_text))
            .or_else(|| hexload_match.and_then(|mapping| self.label_from_hexload(mapping)));

        if let Some(current) = label.as_deref() {
            label = self.normalize_label(current);
        }

        let tag_country = nonempty_extra(&metadata.extra, "country")
            .or_else(|| nonempty_extra(&metadata.extra, "releasecountry"));
        let mut country = tag_country
            .and_then(|value| self.normalize_country(value))
            .or(catalog_country)
            .or_else(|| self.country_from_label(label.as_deref()))
            .or_else(|| hexload_match.and_then(|mapping| self.country_from_hexload(mapping)))
            .or_else(|| self.find_country_in_container_text(&container_text));

        country = country.and_then(|value| self.normalize_country(&value));
        country = normalize_germany_for_year(country, metadata.date.as_deref());

        let is_lp_source = looks_like_lp_source(metadata, &container_text)
            || hexload_match
                .map(hexload_mapping_indicates_lp_source)
                .unwrap_or(false);
        let mut pressing = if is_lp_source {
            hexload_match
                .map(|mapping| normalize_pressing_value(&mapping.value))
                .filter(|value| !value.is_empty())
        } else {
            None
        };

        if let Some(label_value) = label.as_deref() {
            if is_premium_label(label_value) {
                pressing = Some(label_value.to_string());
            }
        }

        if let Some(country_value) = country.as_deref() {
            if country_value != "Japan" && !is_lp_source {
                country = None;
            }
        }

        let resolved = ResolvedLabel {
            label,
            country,
            pressing,
        };

        if resolved.label.is_some() || resolved.country.is_some() || resolved.pressing.is_some() {
            Some(resolved)
        } else {
            None
        }
    }
}

/// Artist name canonicalizer used by the template renderer.
pub struct ArtistCanonicalizer {
    canonical: HashMap<String, String>,
}

impl ArtistCanonicalizer {
    pub fn new() -> Self {
        let canonical = CANONICAL_ARTISTS_REFERENCE
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|artist| (artist.to_lowercase(), artist.to_string()))
            .collect();
        Self { canonical }
    }

    pub fn canonicalize(&self, artist: &str) -> String {
        self.canonical
            .get(&artist.to_lowercase())
            .cloned()
            .unwrap_or_else(|| artist.to_string())
    }
}

impl Default for ArtistCanonicalizer {
    fn default() -> Self {
        Self::new()
    }
}

static ARTIST_CANONICALIZER: LazyLock<ArtistCanonicalizer> =
    LazyLock::new(ArtistCanonicalizer::new);

pub fn canonicalize_artist(artist: &str) -> String {
    ARTIST_CANONICALIZER.canonicalize(artist)
}

static DICTIONARY_LABEL_RESOLVER: LazyLock<DictionaryLabelResolver> =
    LazyLock::new(DictionaryLabelResolver::new);

pub fn dictionary_label_resolver() -> &'static DictionaryLabelResolver {
    &DICTIONARY_LABEL_RESOLVER
}

/// Enrich album metadata with label/pressing info from a resolver.
/// Tag-sourced values in `extra` are never overwritten; the resolver only fills gaps.
pub fn enrich_with_label_info(
    metadata: &mut AlbumMetadata,
    container: &Path,
    resolver: &dyn LabelResolver,
) {
    if let Some(resolved) = resolver.resolve(metadata, container) {
        if let Some(label) = resolved.label {
            metadata.extra.entry("label".to_string()).or_insert(label);
        }
        if let Some(country) = resolved.country {
            metadata
                .extra
                .entry("country".to_string())
                .or_insert(country);
        }
        if let Some(pressing) = resolved.pressing {
            metadata
                .extra
                .entry("pressing".to_string())
                .or_insert(pressing);
        }
    }
}

fn sort_aliases_longest_first(aliases: &mut [Alias]) {
    aliases.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.normalized.cmp(&b.normalized))
    });
}

fn nonempty_extra<'a>(extra: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    extra
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn catalog_value(extra: &BTreeMap<String, String>) -> Option<&str> {
    nonempty_extra(extra, "catalog")
        .or_else(|| nonempty_extra(extra, "catalognumber"))
        .or_else(|| nonempty_extra(extra, "sacd_album_catalog_number"))
}

fn extract_catalog_prefix(value: &str) -> Option<String> {
    let prefix: String = value
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix.to_ascii_uppercase())
    }
}

fn normalize_for_match(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_space = true;

    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            output.push(ch);
            previous_space = false;
        } else if !previous_space {
            output.push(' ');
            previous_space = true;
        }
    }

    output.trim().to_string()
}

fn contains_phrase(normalized_text: &str, normalized_phrase: &str) -> bool {
    if normalized_phrase.is_empty() {
        return false;
    }
    let text = format!(" {normalized_text} ");
    let phrase = format!(" {normalized_phrase} ");
    text.contains(&phrase)
}

fn contains_country_alias_in_container(text: &str, normalized_text: &str, alias: &Alias) -> bool {
    if alias.normalized.chars().count() <= 2 {
        return contains_uppercase_code_token(text, &alias.normalized.to_ascii_uppercase());
    }

    contains_phrase(normalized_text, &alias.normalized)
}

fn contains_uppercase_code_token(text: &str, code: &str) -> bool {
    if code.is_empty() {
        return false;
    }

    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| token == code)
}

fn parse_hexload_mappings() -> Vec<HexloadMapping> {
    let mut mappings = Vec::new();
    let mut rest = HEXLOAD_LABELS_REFERENCE;

    while let Some(index) = rest.find("m.insert(") {
        rest = &rest[index + "m.insert(".len()..];
        let Some((key, after_key)) = parse_rust_string_literal(rest) else {
            continue;
        };

        let after_key = after_key.trim_start();
        let Some(after_comma) = after_key.strip_prefix(',') else {
            rest = after_key;
            continue;
        };

        let after_comma = after_comma.trim_start();
        let Some((value, after_value)) = parse_rust_string_literal(after_comma) else {
            rest = after_comma;
            continue;
        };

        rest = after_value;
        let normalized_key = normalize_for_match(&key);
        if normalized_key.is_empty() {
            continue;
        }

        mappings.push(HexloadMapping {
            score: normalized_key.len(),
            key,
            normalized_key,
            value,
        });
    }

    mappings
}

fn parse_rust_string_literal(input: &str) -> Option<(String, &str)> {
    if !input.starts_with('"') {
        return None;
    }

    let mut output = String::new();
    let mut escaped = false;

    for (offset, ch) in input[1..].char_indices() {
        let absolute_offset = offset + 1;
        if escaped {
            match ch {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                '\\' => output.push('\\'),
                '"' => output.push('"'),
                other => output.push(other),
            }
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return Some((output, &input[absolute_offset + ch.len_utf8()..])),
            other => output.push(other),
        }
    }

    None
}

fn normalize_pressing_value(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|part| *part != "24-96")
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_premium_label(label: &str) -> bool {
    PREMIUM_LABELS.iter().any(|premium| *premium == label)
}

fn digital_download_label(metadata: &AlbumMetadata, container_text: &str) -> Option<String> {
    for (marker, label) in DIGITAL_MARKERS {
        if text_has_marker(container_text, marker) {
            return Some((*label).to_string());
        }
    }

    for (key, value) in &metadata.extra {
        for (marker, label) in DIGITAL_MARKERS {
            if text_has_marker(key, marker) || text_has_marker(value, marker) {
                return Some((*label).to_string());
            }
        }
    }

    None
}

fn text_has_marker(text: &str, marker: &str) -> bool {
    contains_phrase(&normalize_for_match(text), &normalize_for_match(marker))
}

fn looks_like_lp_source(metadata: &AlbumMetadata, container_text: &str) -> bool {
    nonempty_extra(&metadata.extra, "media")
        .map(looks_like_lp_text)
        .unwrap_or(false)
        || looks_like_lp_text(container_text)
}

fn hexload_mapping_indicates_lp_source(mapping: &HexloadMapping) -> bool {
    looks_like_lp_text(&mapping.value)
}

fn looks_like_lp_text(value: &str) -> bool {
    let normalized = normalize_for_match(value);
    ["lp", "vinyl", "180g", "200g"]
        .iter()
        .any(|marker| contains_phrase(&normalized, marker))
}

fn normalize_germany_for_year(country: Option<String>, date: Option<&str>) -> Option<String> {
    let country = country?;
    if !matches!(
        country.as_str(),
        "DE" | "West German" | "German" | "Germany"
    ) {
        return Some(country);
    }

    match date
        .and_then(extract_year_from_date)
        .and_then(|year| year.parse::<u32>().ok())
    {
        Some(year) if year < 1990 => Some("West German".to_string()),
        Some(_) => Some("DE".to_string()),
        None => Some(country),
    }
}

fn extract_year_from_date(date: &str) -> Option<String> {
    let mut run = String::new();
    for ch in date.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
            if run.len() == 4 {
                return Some(run);
            }
        } else {
            run.clear();
        }
    }
    None
}

/// Format keywords that indicate a parenthetical contains metadata,
/// not album title content. Each is independently sufficient to
/// trigger `%TITLE_EXTRA%` extraction.
const FORMAT_KEYWORDS: &[&str] = &[
    "SACD", "DVDA", "DVD-A", "DVD-V", "ISO", "XRCD", "XRCD2", "XRCD24", "SHM", "Hybrid", "Blu-Ray",
    "Blu-ray", "BluRay",
];

/// Check whether a string contains a recognized metadata identifier
/// (catalog prefix + number, format keyword, or premium/audiophile label).
///
/// Used by `%TITLE_EXTRA%` to determine whether a trailing parenthetical
/// in an album name contains metadata that should be stripped.
pub fn contains_metadata_identifier(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();

    // Rule 1: catalog prefix + number pattern (e.g., "SRGS-4504", "UCCQ 1234")
    for &(prefix, _) in CATALOG_COUNTRY_MAPPINGS {
        if upper.contains(prefix) {
            // Check that the prefix is followed (possibly after a separator) by digits
            if let Some(pos) = upper.find(prefix) {
                let after = &upper[pos + prefix.len()..];
                let after_trimmed = after.trim_start_matches(|c: char| c == '-' || c == ' ');
                if after_trimmed.starts_with(|c: char| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
    }

    // Rule 2: format keywords (SACD, DVDA, ISO, etc.)
    for keyword in FORMAT_KEYWORDS {
        let kw_upper = keyword.to_ascii_uppercase();
        if upper.contains(&kw_upper) {
            return true;
        }
    }

    // Rule 3: premium/audiophile label names
    let normalized = normalize_for_match(text);
    for &label in PREMIUM_LABELS {
        let label_norm = normalize_for_match(label);
        if contains_phrase(&normalized, &label_norm) {
            return true;
        }
    }

    // Also check all canonical label variants (Blue Note Tone Poet, Music Matters, etc.)
    for &(canonical, variants) in CANONICAL_LABEL_VARIANTS {
        let canon_norm = normalize_for_match(canonical);
        if contains_phrase(&normalized, &canon_norm) {
            return true;
        }
        for variant in variants {
            let var_norm = normalize_for_match(variant);
            if contains_phrase(&normalized, &var_norm) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn metadata_with_extra(extra: &[(&str, &str)]) -> AlbumMetadata {
        AlbumMetadata {
            extra: extra
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn stub_resolver_returns_none() {
        let metadata = AlbumMetadata::default();
        let resolver = StubLabelResolver;
        assert!(resolver.resolve(&metadata, Path::new("test.iso")).is_none());
    }

    #[test]
    fn enrich_does_nothing_with_stub() {
        let mut metadata = AlbumMetadata::default();
        enrich_with_label_info(&mut metadata, Path::new("test.7z"), &StubLabelResolver);
        assert!(!metadata.extra.contains_key("label"));
        assert!(!metadata.extra.contains_key("country"));
        assert!(!metadata.extra.contains_key("pressing"));
    }

    #[test]
    fn enrich_does_not_override_tag_data() {
        let mut metadata = AlbumMetadata {
            extra: BTreeMap::from([("label".to_string(), "Blue Note".to_string())]),
            ..Default::default()
        };

        struct TestResolver;
        impl LabelResolver for TestResolver {
            fn resolve(
                &self,
                _metadata: &AlbumMetadata,
                _container: &Path,
            ) -> Option<ResolvedLabel> {
                Some(ResolvedLabel {
                    label: Some("MFSL".to_string()),
                    country: Some("US".to_string()),
                    pressing: Some("MFSL LP".to_string()),
                })
            }
        }

        enrich_with_label_info(&mut metadata, Path::new("test.7z"), &TestResolver);

        assert_eq!(metadata.extra.get("label").unwrap(), "Blue Note");
        assert_eq!(metadata.extra.get("country").unwrap(), "US");
        assert_eq!(metadata.extra.get("pressing").unwrap(), "MFSL LP");
    }

    #[test]
    fn enrich_populates_all_fields_when_empty() {
        let mut metadata = AlbumMetadata::default();

        struct TestResolver;
        impl LabelResolver for TestResolver {
            fn resolve(
                &self,
                _metadata: &AlbumMetadata,
                _container: &Path,
            ) -> Option<ResolvedLabel> {
                Some(ResolvedLabel {
                    label: Some("Analogue Productions".to_string()),
                    country: Some("US".to_string()),
                    pressing: Some("AP 45rpm".to_string()),
                })
            }
        }

        enrich_with_label_info(&mut metadata, Path::new("test.iso"), &TestResolver);

        assert_eq!(metadata.extra.get("label").unwrap(), "Analogue Productions");
        assert_eq!(metadata.extra.get("country").unwrap(), "US");
        assert_eq!(metadata.extra.get("pressing").unwrap(), "AP 45rpm");
    }

    #[test]
    fn catalog_prefix_maps_to_country() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("catalog", "UCCQ-1234")]);
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country.as_deref(), Some("Japan"));
    }

    #[test]
    fn unknown_catalog_prefix_returns_none() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("catalog", "UNKNOWN-1234")]);
        assert!(resolver.resolve(&metadata, Path::new("album.7z")).is_none());
    }

    #[test]
    fn label_normalization_uses_canonical_forms() {
        let resolver = DictionaryLabelResolver::new();
        assert_eq!(resolver.normalize_label("MoFi").as_deref(), Some("MFSL"));
        assert_eq!(
            resolver.normalize_label("Blue Note").as_deref(),
            Some("Blue Note")
        );
    }

    #[test]
    fn country_normalization_uses_canonical_forms() {
        let resolver = DictionaryLabelResolver::new();
        assert_eq!(resolver.normalize_country("JPN").as_deref(), Some("Japan"));
        assert_eq!(
            resolver.normalize_country("W. Germany").as_deref(),
            Some("West German")
        );
    }

    #[test]
    fn premium_label_sets_pressing_to_label_and_suppresses_non_lp_country() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("label", "MoFi"), ("catalog", "UDCD-123")]);
        let resolved = resolver
            .resolve(&metadata, Path::new("album CD.7z"))
            .unwrap();
        assert_eq!(resolved.label.as_deref(), Some("MFSL"));
        assert_eq!(resolved.pressing.as_deref(), Some("MFSL"));
        assert_eq!(resolved.country, None);
    }

    #[test]
    fn japan_country_is_preserved_for_any_media() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("catalog", "UCCQ-1234"), ("media", "CD")]);
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country.as_deref(), Some("Japan"));
    }

    #[test]
    fn us_cd_country_is_suppressed() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("label", "DCC"), ("media", "CD")]);
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country, None);
    }

    #[test]
    fn uk_lp_country_is_preserved() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("country", "UK"), ("media", "LP")]);
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country.as_deref(), Some("UK"));
    }

    #[test]
    fn germany_pre_1990_normalizes_to_west_german() {
        let resolver = DictionaryLabelResolver::new();
        let mut metadata = metadata_with_extra(&[("country", "Germany"), ("media", "LP")]);
        metadata.date = Some("1985".to_string());
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country.as_deref(), Some("West German"));
    }

    #[test]
    fn germany_post_1990_normalizes_to_de() {
        let resolver = DictionaryLabelResolver::new();
        let mut metadata = metadata_with_extra(&[("country", "Germany"), ("media", "LP")]);
        metadata.date = Some("2005".to_string());
        let resolved = resolver.resolve(&metadata, Path::new("album.7z")).unwrap();
        assert_eq!(resolved.country.as_deref(), Some("DE"));
    }

    #[test]
    fn web_marker_sets_dd_and_suppresses_country() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("country", "Japan"), ("media", "WEB")]);
        let resolved = resolver
            .resolve(&metadata, Path::new("Artist - Album WEB.7z"))
            .unwrap();
        assert_eq!(resolved.label.as_deref(), Some("DD"));
        assert_eq!(resolved.country, None);
        assert_eq!(resolved.pressing, None);
    }

    #[test]
    fn dictionary_resolver_static_returns_same_instance() {
        assert!(std::ptr::eq(
            dictionary_label_resolver(),
            dictionary_label_resolver()
        ));
    }

    #[test]
    fn hexload_parser_extracts_full_reference_table() {
        let mappings = parse_hexload_mappings();
        assert!(
            mappings.len() >= 300,
            "expected full hexload mapping table, got {} entries",
            mappings.len()
        );
        assert!(mappings.iter().any(|mapping| mapping.key == "QRP"));
        assert!(mappings.iter().any(|mapping| mapping.key == "UK Harvest"));
        assert!(mappings
            .iter()
            .any(|mapping| mapping.key == "Japan Harvest"));
    }

    #[test]
    fn hexload_mapping_resolves_pressing_plant_country_and_pressing() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("media", "LP")]);
        let resolved = resolver
            .resolve(&metadata, Path::new("Artist - Album QRP LP.7z"))
            .unwrap();
        assert_eq!(resolved.country.as_deref(), Some("US"));
        assert_eq!(resolved.pressing.as_deref(), Some("US QRP Press LP"));
    }

    #[test]
    fn hexload_country_takes_precedence_over_short_country_code_words() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("media", "LP")]);
        let resolved = resolver
            .resolve(&metadata, Path::new("The Beatles - Let It Be QRP LP.7z"))
            .unwrap();
        assert_eq!(resolved.country.as_deref(), Some("US"));
        assert_eq!(resolved.pressing.as_deref(), Some("US QRP Press LP"));
    }

    #[test]
    fn short_country_codes_in_container_are_case_sensitive_tokens() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("media", "LP")]);

        let title_word = resolver.resolve(&metadata, Path::new("The Beatles - Let It Be LP.7z"));
        assert!(
            title_word
                .as_ref()
                .and_then(|resolved| resolved.country.as_deref())
                .is_none(),
            "title word 'It' must not resolve as country code IT"
        );

        let explicit_code = resolver
            .resolve(&metadata, Path::new("Artist - Album IT LP.7z"))
            .unwrap();
        assert_eq!(explicit_code.country.as_deref(), Some("IT"));
    }

    #[test]
    fn hexload_lp_value_marks_source_as_lp_when_filename_lacks_lp_marker() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = AlbumMetadata::default();
        let resolved = resolver
            .resolve(&metadata, Path::new("Artist - Album QRP.7z"))
            .unwrap();
        assert_eq!(resolved.country.as_deref(), Some("US"));
        assert_eq!(resolved.pressing.as_deref(), Some("US QRP Press LP"));
    }

    #[test]
    fn hexload_harvest_variants_resolve_country_ambiguity() {
        let resolver = DictionaryLabelResolver::new();
        let metadata = metadata_with_extra(&[("media", "LP")]);

        let uk = resolver
            .resolve(&metadata, Path::new("Artist - Album UK Harvest LP.7z"))
            .unwrap();
        assert_eq!(uk.label.as_deref(), Some("Harvest"));
        assert_eq!(uk.country.as_deref(), Some("UK"));

        let japan = resolver
            .resolve(&metadata, Path::new("Artist - Album Japan Harvest LP.7z"))
            .unwrap();
        assert_eq!(japan.label.as_deref(), Some("Harvest"));
        assert_eq!(japan.country.as_deref(), Some("Japan"));
    }

    #[test]
    fn enrichment_is_repeatable_and_does_not_overwrite_existing_values() {
        let resolver = DictionaryLabelResolver::new();
        let mut metadata = metadata_with_extra(&[
            ("label", "Blue Note"),
            ("catalog", "UCCQ-1234"),
            ("media", "CD"),
        ]);

        enrich_with_label_info(&mut metadata, Path::new("album.7z"), &resolver);
        let once = metadata.extra.clone();
        enrich_with_label_info(&mut metadata, Path::new("album.7z"), &resolver);

        assert_eq!(metadata.extra, once);
        assert_eq!(
            metadata.extra.get("label").map(String::as_str),
            Some("Blue Note")
        );
        assert_eq!(
            metadata.extra.get("country").map(String::as_str),
            Some("Japan")
        );
    }

    #[test]
    fn artist_reference_contains_expected_large_canonical_list() {
        let canonicalizer = ArtistCanonicalizer::new();
        assert!(canonicalizer.canonical.len() >= 2_400);
    }

    #[test]
    fn artist_canonicalizer_returns_known_casing_and_passes_unknowns_through() {
        let canonicalizer = ArtistCanonicalizer::new();
        assert_eq!(canonicalizer.canonicalize("miles davis"), "Miles Davis");
        assert_eq!(canonicalizer.canonicalize("MILES DAVIS"), "Miles Davis");
        assert_eq!(
            canonicalizer.canonicalize("bill evans trio"),
            "Bill Evans Trio"
        );
        assert_eq!(
            canonicalizer.canonicalize("Unknown Artist Not In List"),
            "Unknown Artist Not In List"
        );
    }

    #[test]
    fn metadata_identifier_matches_catalog_prefix_with_digits() {
        assert!(contains_metadata_identifier("SME JSACD SRGS-4504"));
        assert!(contains_metadata_identifier("UCCQ 1234"));
        assert!(contains_metadata_identifier("TOCP-12345"));
    }

    #[test]
    fn metadata_identifier_rejects_catalog_prefix_without_digits() {
        assert!(!contains_metadata_identifier("SRGS"));
        assert!(!contains_metadata_identifier("UCCQ only text"));
    }

    #[test]
    fn metadata_identifier_matches_format_keywords() {
        assert!(contains_metadata_identifier("SACD 2.0"));
        assert!(contains_metadata_identifier("Japan / SHM SACD ISO"));
        assert!(contains_metadata_identifier("Hybrid SACD"));
        assert!(contains_metadata_identifier("DVDA"));
        assert!(contains_metadata_identifier("Blu-Ray"));
    }

    #[test]
    fn metadata_identifier_matches_premium_labels() {
        assert!(contains_metadata_identifier("MFSL LP / 24-96"));
        assert!(contains_metadata_identifier("DCC Compact Classics"));
        assert!(contains_metadata_identifier("Analogue Productions SACD"));
    }

    #[test]
    fn metadata_identifier_rejects_non_metadata() {
        assert!(!contains_metadata_identifier("alternate take"));
        assert!(!contains_metadata_identifier("Mono"));
        assert!(!contains_metadata_identifier("Live at the Apollo"));
        assert!(!contains_metadata_identifier("US"));
        assert!(!contains_metadata_identifier("1st show"));
    }
}
