//! Authoritative catalog-number evidence for advisory pre-emphasis detection.
//!
//! The reference data is embedded directly from
//! `docs/cds-with-preemphasis-shf.csv`. Matching is deliberately exact after
//! punctuation/spacing normalization. The previous family/series regexes were
//! removed because they asserted pre-emphasis for catalog numbers absent from
//! the reference list.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use lazy_static::lazy_static;

use super::metadata::PreemphasisEvidence;
use super::PreemphasisConfidence;

const REFERENCE_CSV: &str = include_str!("../../../docs/cds-with-preemphasis-shf.csv");

/// Whether a catalog result is an exact reference-list hit or a weaker
/// heuristic. The current implementation intentionally emits only `Exact`;
/// retaining the distinction in the public result prevents future callers from
/// flattening a heuristic into exact evidence again.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CatalogMatchQuality {
    Exact,
    Heuristic,
}

/// Where the matched catalog text came from.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CatalogMatchSource {
    Tag,
    Folder,
}

impl CatalogMatchSource {
    fn label(self) -> &'static str {
        match self {
            Self::Tag => "audio tag",
            Self::Folder => "folder name",
        }
    }
}

/// Result of authoritative catalog-based pre-emphasis matching.
#[derive(Debug, Clone)]
pub struct CatalogMatch {
    pub evidence: PreemphasisEvidence,
    pub confidence: PreemphasisConfidence,
    pub quality: CatalogMatchQuality,
    pub source: CatalogMatchSource,
    pub catalog_number: String,
    /// One-based parsed CSV record, including banner/header records.
    pub source_row: usize,
    /// Original authoritative catalog cell from which this alias was derived.
    pub source_catalog_cell: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct CatalogReference {
    display: String,
    normalized: String,
    artist: String,
    title: String,
    source_row: usize,
    source_catalog_cell: String,
    /// Whether this exact alias is distinctive enough to infer from a folder
    /// name. Dedicated catalog tags always require exact normalized equality.
    allow_folder_match: bool,
}

#[derive(Debug, Clone, Copy)]
struct AuditedCatalogAlias {
    display: &'static str,
    allow_folder_match: bool,
}

#[derive(Debug)]
struct CatalogAliasCandidate {
    display: String,
    allow_folder_match: bool,
    audited: bool,
}

/// Reviewed aliases for rows whose catalog cell cannot be expanded safely by a
/// general syntactic rule. Keep these exceptions small and tied to exact row
/// identity; NOTES prose is intentionally not parsed.
struct AuditedCatalogAliasRow {
    artist: &'static str,
    title: &'static str,
    catalog_cell: &'static str,
    positive_aliases: &'static [AuditedCatalogAlias],
    explicit_negative_aliases: &'static [&'static str],
}

const AUDITED_CATALOG_ALIAS_ROWS: &[AuditedCatalogAliasRow] = &[
    AuditedCatalogAliasRow {
        artist: "Ingrid Haebler",
        title: "Mozart: Complete Piano Sonatas",
        catalog_cell: "83689-83693",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "83689",
                allow_folder_match: false,
            },
            AuditedCatalogAlias {
                display: "83691",
                allow_folder_match: false,
            },
            AuditedCatalogAlias {
                display: "83692",
                allow_folder_match: false,
            },
            AuditedCatalogAlias {
                display: "83693",
                allow_folder_match: false,
            },
        ],
        explicit_negative_aliases: &["83690"],
    },
    AuditedCatalogAliasRow {
        artist: "(Compilation)",
        title: "Crème de la crème, Gourmet Selections from Sheffield Lab",
        catalog_cell: "CD-CRM",
        positive_aliases: &[AuditedCatalogAlias {
            display: "CD-CRM",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Merle Haggard & Willie Nelson",
        title: "Pancho & Lefty",
        catalog_cell: "EK 37958 - DIDP 20085",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "EK 37958",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "DIDP 20085",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    // Reviewed bounded multidisc abbreviations. These aliases are tied to the
    // exact source rows rather than teaching the generic parser that every
    // tilde denotes an independently tagged catalog range.
    AuditedCatalogAliasRow {
        artist: "Bruno Walter/New York Philharmonic Orchestra",
        title: "Mahler: Symphony No.2",
        catalog_cell: "56DC131~132",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "56DC131",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "56DC132",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Miles Davis",
        title: "Agharta",
        catalog_cell: "50DP 237~8",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "50DP 237",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "50DP 238",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Miles Davis",
        title: "Pangaea",
        catalog_cell: "50DP 239~40",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "50DP 239",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "50DP 240",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Momoe Yamaguchi",
        title: "33 Singles",
        catalog_cell: "60DH51~2",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "60DH51",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "60DH52",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Pink Floyd",
        title: "The Wall",
        catalog_cell: "50DP 361~2",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "50DP 361",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "50DP 362",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Weather Report",
        title: "0.3541666666666667",
        catalog_cell: "50DP 133~4",
        positive_aliases: &[
            AuditedCatalogAlias {
                display: "50DP 133",
                allow_folder_match: true,
            },
            AuditedCatalogAlias {
                display: "50DP 134",
                allow_folder_match: true,
            },
        ],
        explicit_negative_aliases: &[],
    },
    // Spreadsheet numeric cells exported with a trailing `.0`. Each row below
    // was reviewed individually: the integer is either corroborated by NOTES /
    // MATRIX or is a single-release numeric cell with no disc-selective caveat.
    // Selective or otherwise ambiguous rows stay unexpanded.
    AuditedCatalogAliasRow {
        artist: "A. Nicolet,H.Holliger,C. Jaccottet",
        title: "J.S. Bach / C.P.E. Bach: Trio Sonatas",
        catalog_cell: "70789.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "70789",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Glenn Miller Orchestra, The",
        title: "A Memorial for Glenn Miller, Volume 1",
        catalog_cell: "139201.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "139201",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Gregor Weichert",
        title: "Gregor Weichert- Schubert: Piano Sonatas D 845, 566, and 613",
        catalog_cell: "220542.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "220542",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Jean-Joël Barbier",
        title: "Satie, Œuvres Pour Piano, Volume 1",
        catalog_cell: "149564.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "149564",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Mado Robin",
        title: "S/T (coloratura soprano arias)",
        catalog_cell: "200022.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "200022",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Neil Young and the Shocking Pinks",
        title: "Everybody's Rockin'",
        catalog_cell: "771791.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "771791",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Sugiyama Kiyotaka & Omega Tribe",
        title: "Never Ending Summer",
        catalog_cell: "80006.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "80006",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
    AuditedCatalogAliasRow {
        artist: "Trevor Pinnock",
        title: "Vivaldi, Violin Concertos, Op. 8, Nos. 5-10",
        catalog_cell: "3410.0",
        positive_aliases: &[AuditedCatalogAlias {
            display: "3410",
            allow_folder_match: false,
        }],
        explicit_negative_aliases: &[],
    },
];

#[derive(Debug, Clone)]
struct CatalogNegativeReference {
    normalized: String,
    source_row: usize,
    source_catalog_cell: String,
}

#[derive(Debug)]
struct ReferenceCatalog {
    /// Longest aliases first, so a short exact ID cannot shadow a longer one.
    aliases: Vec<CatalogReference>,
    /// Explicitly reviewed non-PE disc IDs tied to otherwise positive rows.
    explicit_negatives: HashMap<String, CatalogNegativeReference>,
    source_rows: usize,
}

impl ReferenceCatalog {
    fn parse(csv: &str) -> Result<Self, String> {
        let records = parse_csv_records(csv)?;
        let header_index = records
            .iter()
            .position(|row| {
                row.iter()
                    .any(|cell| cell.trim().eq_ignore_ascii_case("CATALOG ID#"))
            })
            .ok_or_else(|| "reference CSV has no CATALOG ID# header".to_string())?;
        let header = &records[header_index];
        let column = |name: &str| {
            header
                .iter()
                .position(|cell| cell.trim().eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("reference CSV has no {name} column"))
        };
        let artist_column = column("ARTIST")?;
        let title_column = column("RELEASE TITLE")?;
        let catalog_column = column("CATALOG ID#")?;
        let flag_column = column("PE FLAG")?;
        let notes_column = column("NOTES")?;

        let mut by_normalized = HashMap::<String, CatalogReference>::new();
        let mut explicit_negatives = HashMap::<String, CatalogNegativeReference>::new();
        let mut source_rows = 0usize;
        for (row_offset, row) in records.iter().skip(header_index + 1).enumerate() {
            let source_row = header_index + row_offset + 2;
            let catalog_cell = row.get(catalog_column).map(String::as_str).unwrap_or("").trim();
            if catalog_cell.is_empty() {
                continue;
            }
            let flag = row.get(flag_column).map(String::as_str).unwrap_or("");
            let notes = row.get(notes_column).map(String::as_str).unwrap_or("");
            if row_explicitly_denies_preemphasis(flag, notes) {
                continue;
            }

            let artist = row
                .get(artist_column)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let title = row
                .get(title_column)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let audited = audited_aliases_for_row(&artist, &title, catalog_cell);
            let candidates = if let Some(audited) = audited {
                for alias in audited.explicit_negative_aliases {
                    let normalized = normalize_catalog(alias);
                    if reference_alias_is_usable(&normalized) {
                        explicit_negatives.insert(
                            normalized.clone(),
                            CatalogNegativeReference {
                                normalized,
                                source_row,
                                source_catalog_cell: catalog_cell.to_string(),
                            },
                        );
                    }
                }
                audited
                    .positive_aliases
                    .iter()
                    .map(|alias| CatalogAliasCandidate {
                        display: alias.display.to_string(),
                        allow_folder_match: alias.allow_folder_match,
                        audited: true,
                    })
                    .collect::<Vec<_>>()
            } else {
                split_catalog_cell(catalog_cell)
                    .into_iter()
                    .flat_map(|cell_value| expand_compact_catalog_range(&cell_value))
                    .map(|display| CatalogAliasCandidate {
                        allow_folder_match: default_folder_match_eligibility(
                            &normalize_catalog(&display),
                        ),
                        display,
                        audited: false,
                    })
                    .collect::<Vec<_>>()
            };

            let mut accepted_from_row = false;
            for candidate in candidates {
                let CatalogAliasCandidate {
                    display,
                    allow_folder_match,
                    audited,
                } = candidate;
                let normalized = normalize_catalog(&display);
                let usable = if audited {
                    audited_reference_alias_is_usable(&normalized)
                } else {
                    reference_alias_is_usable(&normalized)
                };
                if !usable {
                    continue;
                }
                accepted_from_row = true;
                let reference = CatalogReference {
                    display,
                    normalized: normalized.clone(),
                    artist: artist.clone(),
                    title: title.clone(),
                    source_row,
                    source_catalog_cell: catalog_cell.to_string(),
                    allow_folder_match,
                };
                if audited {
                    // Reviewed aliases are authoritative over any mechanically
                    // derived duplicate elsewhere in the spreadsheet.
                    by_normalized.insert(normalized, reference);
                } else {
                    by_normalized.entry(normalized).or_insert(reference);
                }
            }
            if accepted_from_row {
                source_rows += 1;
            }
        }

        let mut aliases = by_normalized.into_values().collect::<Vec<_>>();
        aliases.sort_by(|a, b| {
            b.normalized
                .len()
                .cmp(&a.normalized.len())
                .then_with(|| a.normalized.cmp(&b.normalized))
        });
        if aliases.is_empty() {
            return Err("reference CSV produced no usable catalog aliases".to_string());
        }
        Ok(Self {
            aliases,
            explicit_negatives,
            source_rows,
        })
    }

    fn is_explicit_negative_tag(&self, text: &str) -> bool {
        let normalized = normalize_catalog(text);
        self.explicit_negatives.contains_key(&normalized)
    }

    fn find_in_text(
        &self,
        text: &str,
        source: CatalogMatchSource,
    ) -> Option<(&CatalogReference, Range<usize>)> {
        let normalized = NormalizedText::new(text);
        if normalized.value.is_empty() {
            return None;
        }

        // A dedicated catalog tag is authoritative only when its complete
        // normalized value is present in the reference data. Treating a known
        // ID as a substring of a larger tag recreates the false-positive class
        // this table is intended to remove.
        if source == CatalogMatchSource::Tag {
            if self.explicit_negatives.contains_key(&normalized.value) {
                return None;
            }
            let reference = self
                .aliases
                .iter()
                .find(|reference| reference.normalized == normalized.value)?;
            return Some((reference, 0..text.len()));
        }

        for reference in &self.aliases {
            if source == CatalogMatchSource::Folder && !reference.allow_folder_match {
                continue;
            }
            for (start, _) in normalized.value.match_indices(&reference.normalized) {
                let end = start + reference.normalized.len();
                let original = normalized.original_range(start, end)?;
                if has_catalog_boundaries(text, original.start, original.end) {
                    return Some((reference, original));
                }
            }
        }
        None
    }
}

lazy_static! {
    static ref REFERENCE_CATALOG: ReferenceCatalog = ReferenceCatalog::parse(REFERENCE_CSV)
        .expect("bundled pre-emphasis reference CSV must remain parseable");
}

fn row_explicitly_denies_preemphasis(flag: &str, notes: &str) -> bool {
    let flag = flag.to_ascii_lowercase();
    let notes = notes.to_ascii_lowercase();
    flag.contains("no pe")
        || notes.contains("no pre-emphasis")
        || notes.contains("hasn't actually been applied")
        || notes.contains("has not actually been applied")
}

fn audited_aliases_for_row(
    artist: &str,
    title: &str,
    catalog_cell: &str,
) -> Option<&'static AuditedCatalogAliasRow> {
    AUDITED_CATALOG_ALIAS_ROWS.iter().find(|row| {
        row.artist == artist && row.title == title && row.catalog_cell == catalog_cell
    })
}

fn split_catalog_cell(cell: &str) -> Vec<String> {
    let mut values = Vec::new();
    for segment in cell.split(|ch| matches!(ch, '\n' | '\r' | ',' | ';')) {
        // A spaced slash is used as a separator in the spreadsheet. Compact
        // slashes (for example COCO-70777/8 or D/CD 3041) are part of an ID.
        for value in segment.split(" / ") {
            let value = value.trim();
            if !value.is_empty() {
                values.push(value.to_string());
            }
        }
    }
    values
}

fn expand_compact_catalog_range(value: &str) -> Vec<String> {
    let mut values = vec![value.trim().to_string()];
    let Some((left, abbreviated_end)) = value.rsplit_once('/') else {
        return values;
    };
    let abbreviated_end = abbreviated_end.trim();
    if abbreviated_end.is_empty()
        || !abbreviated_end.bytes().all(|byte| byte.is_ascii_digit())
    {
        return values;
    }

    let left = left.trim_end();
    let digit_start = left
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (!ch.is_ascii_digit()).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let digits = &left[digit_start..];
    if digits.is_empty() || abbreviated_end.len() > digits.len() {
        return values;
    }

    let suffix_width = abbreviated_end.len();
    let split = digits.len() - suffix_width;
    let common_digits = &digits[..split];
    let Some(start) = digits[split..].parse::<u32>().ok() else {
        return values;
    };
    let Some(end) = abbreviated_end.parse::<u32>().ok() else {
        return values;
    };
    // Spreadsheet abbreviations such as COCO-70777/8, UKCD 2006/7, and
    // CDA66561/3 denote a bounded run sharing the omitted leading digits.
    // Refuse reversed or implausibly large expansions rather than guessing.
    if end < start || end - start > 99 {
        return values;
    }

    let prefix = &left[..digit_start];
    for suffix in start..=end {
        values.push(format!(
            "{prefix}{common_digits}{suffix:0suffix_width$}",
            suffix_width = suffix_width,
        ));
    }
    values.sort();
    values.dedup();
    values
}

fn reference_alias_is_usable(normalized: &str) -> bool {
    normalized.len() >= 3
        && normalized.bytes().any(|byte| byte.is_ascii_digit())
        && normalized.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn audited_reference_alias_is_usable(normalized: &str) -> bool {
    normalized.len() >= 3 && normalized.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn default_folder_match_eligibility(normalized: &str) -> bool {
    normalized.len() >= 5
        && normalized.bytes().any(|byte| byte.is_ascii_alphabetic())
        && normalized.bytes().any(|byte| byte.is_ascii_digit())
}

/// Normalize punctuation and spacing while retaining only ASCII catalog
/// identity. The reference sheet and common CD catalog formats are ASCII.
fn normalize_catalog(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

struct NormalizedText {
    value: String,
    spans: Vec<Range<usize>>,
}

impl NormalizedText {
    fn new(text: &str) -> Self {
        let mut value = String::new();
        let mut spans = Vec::new();
        for (start, ch) in text.char_indices() {
            if ch.is_ascii_alphanumeric() {
                value.push(ch.to_ascii_uppercase());
                spans.push(start..start + ch.len_utf8());
            }
        }
        Self { value, spans }
    }

    fn original_range(&self, start: usize, end: usize) -> Option<Range<usize>> {
        if start >= end || end > self.spans.len() {
            return None;
        }
        Some(self.spans[start].start..self.spans[end - 1].end)
    }
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
    let start = text[byte_index..]
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(byte_index + offset))?;
    if !text[start..].chars().next()?.is_ascii_alphanumeric() {
        return None;
    }
    let end = text[start..]
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_ascii_alphanumeric()).then_some(start + offset))
        .unwrap_or(text.len());
    Some(&text[start..end])
}

fn is_probable_cd_year(token: &str) -> bool {
    token.len() == 4
        && token.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(&token[..2], "19" | "20")
}

fn is_probable_short_suffix(token: &str) -> bool {
    match token.as_bytes() {
        [single] => single.is_ascii_alphabetic(),
        [first, second] => {
            first.is_ascii_alphanumeric()
                && second.is_ascii_alphanumeric()
                && (first.is_ascii_alphabetic() || second.is_ascii_alphabetic())
                && (first.is_ascii_digit() || second.is_ascii_digit())
        }
        _ => false,
    }
}

fn has_catalog_boundaries(text: &str, start: usize, end: usize) -> bool {
    if previous_char(text, start).is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        return false;
    }
    match next_char(text, end) {
        None => true,
        Some(ch) if ch.is_ascii_alphanumeric() => false,
        Some('-' | '.' | '·' | '/') => match next_char_after_first(text, end) {
            None => true,
            Some(ch) if ch.is_whitespace() => true,
            Some(ch) => !ch.is_ascii_alphanumeric(),
        },
        Some(ch) if ch.is_whitespace() => match next_ascii_token(text, end) {
            None => true,
            Some(token) if is_probable_cd_year(token) => true,
            Some(token) if is_probable_short_suffix(token) => false,
            Some(token) => !token.as_bytes()[0].is_ascii_digit(),
        },
        Some(_) => true,
    }
}

fn freeform_catalog_values(audio_path: &Path) -> Vec<String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let Ok(tagged) = lofty::read_from_path(audio_path) else {
        return Vec::new();
    };
    let freeform_keys = [
        "CATALOGNUMBER",
        "CatalogNumber",
        "CATALOG_NUMBER",
        "Catalog_Number",
        "CATALOG NO",
        "Catalog No",
        "CATALOGNO",
        "CatalogNo",
        "CATALOG #",
        "Catalog #",
        "CATALOG",
        "Catalog",
        "CATNO",
        "CatNo",
        "CAT NO",
        "Cat No",
        "CATALOGUE NUMBER",
        "Catalogue Number",
        "CATALOGUE NO",
        "Catalogue No",
        "CATALOGUE_NO",
        "Catalogue_No",
        "CATALOGUE",
        "Catalogue",
    ];
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for tag in tagged.tags() {
        if let Some(value) = tag.get_string(&ItemKey::CatalogNumber) {
            if seen.insert(value.to_string()) {
                values.push(value.to_string());
            }
        }
        for key in freeform_keys {
            for spelling in [key.to_string(), key.to_ascii_lowercase()] {
                if let Some(value) = tag.get_string(&ItemKey::Unknown(spelling)) {
                    if seen.insert(value.to_string()) {
                        values.push(value.to_string());
                    }
                }
            }
        }
    }
    values
}

fn match_text(text: &str, source: CatalogMatchSource) -> Option<CatalogMatch> {
    let (reference, range) = REFERENCE_CATALOG.find_in_text(text, source)?;
    let raw = text[range].trim().to_string();
    let confidence = match source {
        CatalogMatchSource::Tag => PreemphasisConfidence::StrongCandidate,
        CatalogMatchSource::Folder => PreemphasisConfidence::Possible,
    };
    let release = match (reference.artist.trim(), reference.title.trim()) {
        ("", "") => reference.display.clone(),
        ("", title) => title.to_string(),
        (artist, "") => artist.to_string(),
        (artist, title) => format!("{artist} - {title}"),
    };
    Some(CatalogMatch {
        evidence: PreemphasisEvidence::CatalogExact,
        confidence,
        quality: CatalogMatchQuality::Exact,
        source,
        catalog_number: raw,
        source_row: reference.source_row,
        source_catalog_cell: reference.source_catalog_cell.clone(),
        detail: format!(
            "exact authoritative catalog {} from {} matches {}",
            reference.display,
            source.label(),
            release
        ),
    })
}

fn match_catalog_sources(tag_values: &[String], folder: Option<&str>) -> Option<CatalogMatch> {
    // An explicitly reviewed negative disc ID is authoritative and suppresses
    // weaker folder inference for the containing multidisc release.
    if tag_values
        .iter()
        .any(|value| REFERENCE_CATALOG.is_explicit_negative_tag(value))
    {
        return None;
    }
    for value in tag_values {
        if let Some(matched) = match_text(value, CatalogMatchSource::Tag) {
            return Some(matched);
        }
    }
    folder.and_then(|folder| match_text(folder, CatalogMatchSource::Folder))
}

/// Check tags first, then the containing folder, for an exact catalog ID from
/// the bundled authoritative list. No prefix/range/series inference is made.
pub fn check_catalog_evidence(audio_path: &Path) -> Option<CatalogMatch> {
    let tag_values = freeform_catalog_values(audio_path);
    let folder = audio_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    match_catalog_sources(&tag_values, folder)
}

fn parse_csv_records(input: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = input.chars().peekable();
    let mut quoted = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut row));
            }
            '\r' if !quoted && chars.peek() == Some(&'\n') => {}
            _ => field.push(ch),
        }
    }
    if quoted {
        return Err("reference CSV ends inside a quoted field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        records.push(row);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_punctuation_insensitive_but_not_prefix_based() {
        assert_eq!(normalize_catalog("35DP-150"), "35DP150");
        assert_eq!(normalize_catalog("35·8P-4"), "358P4");
        assert_eq!(normalize_catalog("D/CD 3041"), "DCD3041");
    }

    #[test]
    fn bundled_reference_is_auditable_and_excludes_explicit_negative_row() {
        assert!(REFERENCE_CATALOG.source_rows >= 590);
        assert!(REFERENCE_CATALOG.aliases.len() >= 500);
        assert!(match_text("35DP 150", CatalogMatchSource::Tag).is_some());
        assert!(match_text("VDJ-1146", CatalogMatchSource::Tag).is_some());
        assert!(match_text("477902 2", CatalogMatchSource::Tag).is_none());
    }

    #[test]
    fn every_embedded_reference_alias_round_trips_as_an_exact_tag_match() {
        for reference in &REFERENCE_CATALOG.aliases {
            let matched = match_text(&reference.display, CatalogMatchSource::Tag)
                .unwrap_or_else(|| panic!("reference alias did not match: {}", reference.display));
            assert_eq!(matched.quality, CatalogMatchQuality::Exact);
            assert_eq!(matched.source, CatalogMatchSource::Tag);
            assert_eq!(normalize_catalog(&matched.catalog_number), reference.normalized);
            assert_eq!(matched.confidence, PreemphasisConfidence::StrongCandidate);
        }
    }

    #[test]
    fn former_series_near_misses_do_not_match() {
        for value in [
            "35DP-999",
            "50DP-999",
            "32DP-999",
            "35DC-999",
            "56DC-999",
            "35DH-999",
            "CP35-3999",
            "CDV-9999",
            "AGCD-999",
        ] {
            assert!(
                match_text(value, CatalogMatchSource::Tag).is_none(),
                "unsupported series neighbor matched: {value}"
            );
        }
    }

    #[test]
    fn exact_match_rejects_embedded_and_suffixed_catalogs() {
        for value in [
            "foo35DP-150",
            "35DP-150bar",
            "35DP-150-A",
            "35DP-150 A1",
            "35DP-150/1",
            "catalog 35DP-150",
        ] {
            assert!(match_text(value, CatalogMatchSource::Tag).is_none(), "{value}");
        }
        assert!(match_text("35DP-150 1982 Japan", CatalogMatchSource::Folder).is_some());
        assert!(match_text("Album 35DP-150/1", CatalogMatchSource::Folder).is_none());
    }

    #[test]
    fn compact_spreadsheet_ranges_expand_only_to_provably_implied_ids() {
        assert_eq!(
            expand_compact_catalog_range("COCO-70777/8"),
            vec!["COCO-70777", "COCO-70777/8", "COCO-70778"]
        );
        assert!(match_text("COCO-70777", CatalogMatchSource::Tag).is_some());
        assert!(match_text("COCO-70778", CatalogMatchSource::Tag).is_some());
        assert!(match_text("COCO-70779", CatalogMatchSource::Tag).is_none());

        assert!(match_text("CDA66561", CatalogMatchSource::Tag).is_some());
        assert!(match_text("CDA66562", CatalogMatchSource::Tag).is_some());
        assert!(match_text("CDA66563", CatalogMatchSource::Tag).is_some());
        assert!(match_text("CDA66564", CatalogMatchSource::Tag).is_none());
        assert_eq!(
            expand_compact_catalog_range("ADEH/CD/780"),
            vec!["ADEH/CD/780"]
        );
    }

    #[test]
    fn folder_evidence_is_exact_but_lower_confidence_than_a_catalog_tag() {
        let tag = match_text("35DP-150", CatalogMatchSource::Tag).expect("tag match");
        let folder = match_text(
            "Willie Nelson - Angel Eyes - 35DP-150",
            CatalogMatchSource::Folder,
        )
        .expect("folder match");
        assert_eq!(tag.quality, CatalogMatchQuality::Exact);
        assert_eq!(folder.quality, CatalogMatchQuality::Exact);
        assert_eq!(tag.confidence, PreemphasisConfidence::StrongCandidate);
        assert_eq!(folder.confidence, PreemphasisConfidence::Possible);
    }

    #[test]
    fn ambiguous_short_and_numeric_reference_ids_are_tag_only() {
        assert!(match_text("CD-2", CatalogMatchSource::Tag).is_some());
        assert!(match_text("Album CD-2", CatalogMatchSource::Folder).is_none());
        assert!(match_text("831077-2", CatalogMatchSource::Tag).is_some());
        assert!(match_text("Album 831077-2", CatalogMatchSource::Folder).is_none());
    }

    #[test]
    fn audited_haebler_multidisc_aliases_honor_disc_specific_notes() {
        let audited = audited_aliases_for_row(
            "Ingrid Haebler",
            "Mozart: Complete Piano Sonatas",
            "83689-83693",
        )
        .expect("reviewed alias row");
        assert_eq!(audited.explicit_negative_aliases, &["83690"]);
        let negative = REFERENCE_CATALOG
            .explicit_negatives
            .get("83690")
            .expect("reviewed negative alias");
        assert_eq!(negative.normalized, "83690");
        assert_eq!(negative.source_row, 229);
        assert_eq!(negative.source_catalog_cell, "83689-83693");

        for catalog in ["83689", "83691", "83692", "83693"] {
            let matched = match_text(catalog, CatalogMatchSource::Tag)
                .unwrap_or_else(|| panic!("audited disc alias did not match: {catalog}"));
            assert_eq!(matched.source_catalog_cell, "83689-83693");
            assert_eq!(matched.source_row, 229);
            assert!(matched.detail.contains("Ingrid Haebler"));
        }

        for catalog in ["83688", "83690", "83694", "83689-83693"] {
            assert!(
                match_text(catalog, CatalogMatchSource::Tag).is_none(),
                "non-PE or composite catalog matched: {catalog}",
            );
        }

        let negative_disc = vec!["83690".to_string()];
        assert!(match_catalog_sources(
            &negative_disc,
            Some("Ingrid Haebler - Mozart - 83689-83693"),
        )
        .is_none());
    }

    #[test]
    fn audited_sheffield_lab_alpha_catalog_is_exact_tag_only() {
        let matched = match_text("CD-CRM", CatalogMatchSource::Tag)
            .expect("reviewed alphabetic catalog tag should match");
        assert_eq!(matched.catalog_number, "CD-CRM");
        assert_eq!(matched.source_row, 642);
        assert_eq!(matched.source_catalog_cell, "CD-CRM");
        assert!(matched.detail.contains("Sheffield Lab"));

        assert!(match_text("CD CRM", CatalogMatchSource::Tag).is_some());
        assert!(match_text(
            "Compilation - Creme de la creme - CD CRM",
            CatalogMatchSource::Folder,
        )
        .is_none());
    }

    #[test]
    fn audited_pancho_and_lefty_catalogs_match_independently() {
        for catalog in ["EK 37958", "DIDP 20085"] {
            let matched = match_text(catalog, CatalogMatchSource::Tag)
                .unwrap_or_else(|| panic!("reviewed catalog alias did not match: {catalog}"));
            assert_eq!(matched.source_row, 377);
            assert_eq!(matched.source_catalog_cell, "EK 37958 - DIDP 20085");
            assert!(matched.detail.contains("Pancho & Lefty"));
        }

        for catalog in ["EK 37959", "DIDP 20086"] {
            assert!(
                match_text(catalog, CatalogMatchSource::Tag).is_none(),
                "neighboring catalog matched: {catalog}",
            );
        }
    }

    #[test]
    fn audited_tilde_ranges_match_only_reviewed_disc_catalogs() {
        let cases = [
            ("56DC131~132", 112usize, &["56DC131", "56DC132"][..]),
            ("50DP 237~8", 388usize, &["50DP 237", "50DP 238"][..]),
            ("50DP 239~40", 394usize, &["50DP 239", "50DP 240"][..]),
            ("60DH51~2", 402usize, &["60DH51", "60DH52"][..]),
            ("50DP 361~2", 464usize, &["50DP 361", "50DP 362"][..]),
            ("50DP 133~4", 595usize, &["50DP 133", "50DP 134"][..]),
        ];

        for (source_cell, source_row, aliases) in cases {
            for alias in aliases {
                let matched = match_text(alias, CatalogMatchSource::Tag)
                    .unwrap_or_else(|| panic!("reviewed tilde alias did not match: {alias}"));
                assert_eq!(matched.source_row, source_row, "{alias}");
                assert_eq!(matched.source_catalog_cell, source_cell, "{alias}");
            }
            assert!(
                match_text(source_cell, CatalogMatchSource::Tag).is_none(),
                "composite tilde cell unexpectedly matched: {source_cell}",
            );
        }

        for neighbor in ["50DP236", "50DP241", "60DH53", "56DC133"] {
            assert!(
                match_text(neighbor, CatalogMatchSource::Tag).is_none(),
                "neighboring catalog matched: {neighbor}",
            );
        }

        // This row remains deliberately unexpanded: its cell may identify the
        // combined release rather than seven independently proven disc IDs.
        assert!(match_text("CD-14~20", CatalogMatchSource::Tag).is_some());
        assert!(match_text("CD-14", CatalogMatchSource::Tag).is_none());
        assert!(match_text("CD-20", CatalogMatchSource::Tag).is_none());
    }

    #[test]
    fn audited_decimal_export_aliases_strip_only_corroborated_spreadsheet_suffixes() {
        let matched = match_text("80006", CatalogMatchSource::Tag)
            .expect("reviewed integer catalog should match");
        assert_eq!(matched.source_row, 529);
        assert_eq!(matched.source_catalog_cell, "80006.0");
        assert!(matched.detail.contains("Never Ending Summer"));

        for value in ["80007", "800060"] {
            assert!(
                match_text(value, CatalogMatchSource::Tag).is_none(),
                "incorrect decimal-export neighbor matched: {value}",
            );
        }

        // Every other non-selective decimal export was reviewed explicitly;
        // none relies on a generic suffix-stripping rule.
        for (alias, source_cell, source_row) in [
            ("70789", "70789.0", 21usize),
            ("139201", "139201.0", 195usize),
            ("220542", "220542.0", 201usize),
            ("149564", "149564.0", 243usize),
            ("200022", "200022.0", 346usize),
            ("771791", "771791.0", 424usize),
            ("3410", "3410.0", 577usize),
        ] {
            let matched = match_text(alias, CatalogMatchSource::Tag)
                .unwrap_or_else(|| panic!("reviewed decimal alias did not match: {alias}"));
            assert_eq!(matched.source_row, source_row, "{alias}");
            assert_eq!(matched.source_catalog_cell, source_cell, "{alias}");
        }

        // Do not generically strip `.0`: this multidisc row contains explicit
        // non-PE discs and cannot support a release-wide positive alias.
        assert!(match_text("92005", CatalogMatchSource::Tag).is_none());
    }

    #[test]
    fn every_audited_alias_row_exists_and_preserves_csv_provenance() {
        let records = parse_csv_records(REFERENCE_CSV).expect("parse reference CSV");
        for audited in AUDITED_CATALOG_ALIAS_ROWS {
            let source_row = records
                .iter()
                .position(|row| {
                    row.iter().any(|cell| cell.trim() == audited.artist)
                        && row.iter().any(|cell| cell.trim() == audited.title)
                        && row.iter().any(|cell| cell.trim() == audited.catalog_cell)
                })
                .map(|index| index + 1)
                .unwrap_or_else(|| {
                    panic!(
                        "audited catalog row missing from CSV: {} / {} / {}",
                        audited.artist, audited.title, audited.catalog_cell,
                    )
                });

            for alias in audited.positive_aliases {
                let matched = match_text(alias.display, CatalogMatchSource::Tag)
                    .unwrap_or_else(|| panic!("audited alias did not match: {}", alias.display));
                assert_eq!(matched.source_row, source_row);
                assert_eq!(matched.source_catalog_cell, audited.catalog_cell);
                let folder = format!("Album - {}", alias.display);
                assert_eq!(
                    match_text(&folder, CatalogMatchSource::Folder).is_some(),
                    alias.allow_folder_match,
                    "folder eligibility drifted for {}",
                    alias.display,
                );
            }
        }
    }

    #[test]
    fn csv_parser_handles_embedded_commas_quotes_and_newlines() {
        let rows = parse_csv_records("A,B\n\"x,y\",\"line 1\nline \"\"2\"\"\"\n")
            .expect("parse fixture");
        assert_eq!(rows[1][0], "x,y");
        assert_eq!(rows[1][1], "line 1\nline \"2\"");
    }
}
