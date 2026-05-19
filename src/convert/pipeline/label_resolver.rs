//! Label/pressing resolution interface for naming templates.
//!
//! The `LabelResolver` trait provides an extensible hook for resolving
//! `%LABEL%`, `%COUNTRY%`, and `%PRESSING%` template variables from
//! album metadata and container paths. The stub implementation returns
//! `None` for all inputs; a future heuristic engine (catalog-number
//! dictionaries, folder-name pattern matching) can replace it.
//!
//! Resolution chain: tag metadata (already in `extra`) wins over
//! resolver output. The enrichment function uses `entry().or_insert()`
//! so existing tag-sourced values are never overwritten.

use std::path::Path;

use super::types::AlbumMetadata;

/// Resolved label/pressing information from a `LabelResolver`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedLabel {
    pub label: Option<String>,
    pub country: Option<String>,
    pub pressing: Option<String>,
}

/// Trait for resolving label, country, and pressing information from
/// album metadata and container path. Implementations may use catalog
/// number dictionaries, folder-name heuristics, or external lookups.
pub trait LabelResolver: Send + Sync {
    fn resolve(&self, metadata: &AlbumMetadata, container: &Path) -> Option<ResolvedLabel>;
}

/// Stub resolver that always returns `None`. Placeholder until the
/// heuristic engine (catalog-number dictionaries) is implemented.
pub struct StubLabelResolver;

impl LabelResolver for StubLabelResolver {
    fn resolve(&self, _metadata: &AlbumMetadata, _container: &Path) -> Option<ResolvedLabel> {
        None
    }
}

/// Enrich album metadata with label/pressing info from a resolver.
/// Tag-sourced values in `extra` are never overwritten — the resolver
/// only fills gaps.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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

        // A resolver that returns all three fields
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

        // Tag-sourced "Blue Note" must NOT be overwritten
        assert_eq!(metadata.extra.get("label").unwrap(), "Blue Note");
        // But country and pressing should be populated
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
}
