//! Strict DSD settings and Reference-intent sentinel tests.

use tonepoet_pipeline::{
    fingerprint::settings_snapshot_fingerprint_v2, DbNano, DsdGeneralExportLevel,
    DsdGeneralReconstruction, DsdSettings, DsdSourcePathway, PipelineSettings,
    SampleGainPolicy, TruePeakScanTier, TruePeakScope,
};

fn db(value: &str) -> DbNano {
    value.parse().expect("valid dB fixture")
}

#[test]
fn reference_constructor_selects_reference_without_schema_origin_state() {
    let settings = DsdSettings::reference();
    assert_eq!(settings.from_dsd.pathway, DsdSourcePathway::Reference);
    assert_eq!(settings.gain_policy(), SampleGainPolicy::Off);
}

#[test]
fn general_and_reference_pathway_changes_are_visible_to_audit_snapshot() {
    let general = PipelineSettings::default();
    let mut reference = general.clone();
    reference.dsd = DsdSettings::reference();
    assert_ne!(
        settings_snapshot_fingerprint_v2(&general),
        settings_snapshot_fingerprint_v2(&reference)
    );
}

#[test]
fn ordinary_general_controls_remain_independent_from_reference_delivery_controls() {
    let mut settings = PipelineSettings::default();
    settings.dsd.from_dsd.pathway = DsdSourcePathway::Reference;
    settings.dsd.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
    settings.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::Native;
    settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
        target_dbtp: db("-0.500000000"),
        scope: TruePeakScope::Album,
        scan: TruePeakScanTier::Reference,
    });

    // Persisted ordinary preferences may be retained for later editing, but
    // Reference pathway selection remains a separate explicit authority.
    assert_eq!(settings.dsd.from_dsd.pathway, DsdSourcePathway::Reference);
    assert_eq!(settings.dsd.true_peak_scope(), Some(TruePeakScope::Album));
}

#[test]
fn strict_dsd_wire_has_directional_objects_and_no_origin_or_version_selector() {
    let value = serde_json::to_value(DsdSettings::reference()).expect("serialize DSD settings");
    let object = value.as_object().expect("DSD settings object");
    assert!(object.contains_key("pcm_to_dsd"));
    assert!(object.contains_key("from_dsd"));
    assert!(object.contains_key("general_from_dsd"));
    for retired in ["origin", "schema_origin", "settings_version", "legacy"] {
        assert!(!object.contains_key(retired), "retired selector {retired} leaked into wire");
    }
}
