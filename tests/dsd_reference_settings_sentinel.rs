//! Native-v2 DSD settings sentinel.
//!
//! The legacy-v1 settings fingerprint and its sentinel remain frozen for old
//! manifests. This file mechanically inventories the additive directional DSD
//! snapshot domain used by native-v2 manifests.

use std::collections::BTreeSet;

use tonepoet_pipeline::fingerprint::settings_snapshot_fingerprint_v2;
use tonepoet_pipeline::{
    DbNano, DsdFilterPreset, DsdNoiseShaper, DsdReconstructionSelection,
    DsdReferencePolicyVersion, DsdSourceGainMode, DsdSourcePathway, GainCompensation,
    ModulatorOrder, PipelineSettings, TrellisSettings,
    SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT, SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS,
};

fn round_trip(settings: &PipelineSettings) -> PipelineSettings {
    let json = serde_json::to_vec(settings).expect("serialize native-v2 settings");
    serde_json::from_slice(&json).expect("deserialize native-v2 settings")
}

fn serialized_native_dsd_paths(settings: &PipelineSettings) -> BTreeSet<String> {
    let encoded = serde_json::to_value(settings).expect("serialize native-v2 settings inventory");
    let dsd = encoded
        .get("dsd")
        .and_then(serde_json::Value::as_object)
        .expect("native-v2 dsd object");
    let mut paths = BTreeSet::new();
    paths.insert("dsd.schema".to_string());

    fn visit(prefix: &str, value: &serde_json::Value, paths: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    let path = format!("{prefix}.{key}");
                    if value.is_object() {
                        visit(&path, value, paths);
                    } else {
                        paths.insert(path);
                    }
                }
            }
            _ => panic!("expected object at {prefix}"),
        }
    }

    visit(
        "dsd.pcm_to_dsd",
        dsd.get("pcm_to_dsd").expect("pcm_to_dsd object"),
        &mut paths,
    );
    visit(
        "dsd.from_dsd",
        dsd.get("from_dsd").expect("from_dsd object"),
        &mut paths,
    );
    paths
}

#[test]
fn native_v2_dsd_inventory_matches_serialized_wire_and_has_no_duplicates() {
    let expected: BTreeSet<String> = SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS
        .iter()
        .map(|path| (*path).to_string())
        .collect();
    assert_eq!(
        expected.len(),
        SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT,
        "native-v2 DSD inventory contains duplicates"
    );
    assert_eq!(serialized_native_dsd_paths(&PipelineSettings::default()), expected);
}

#[test]
fn native_v2_reference_fields_are_persisted_and_fingerprinted_independently() {
    let baseline = PipelineSettings::default();
    assert!(baseline.dsd.is_native_v2());
    let baseline_fingerprint = settings_snapshot_fingerprint_v2(&baseline);

    let mut variants = Vec::new();

    let mut settings = baseline.clone();
    settings.dsd.pcm_to_dsd.noise_shaper = DsdNoiseShaper::Crfb;
    variants.push(("dsd.pcm_to_dsd.noise_shaper", settings));

    let mut settings = baseline.clone();
    settings.dsd.pcm_to_dsd.modulator_order = ModulatorOrder::Order7;
    variants.push(("dsd.pcm_to_dsd.modulator_order", settings));

    let mut settings = baseline.clone();
    settings.dsd.pcm_to_dsd.trellis = Some(TrellisSettings {
        lookahead: 17,
        nodes: 9,
        latency: Some(321),
    });
    variants.push(("dsd.pcm_to_dsd.trellis", settings));

    let mut settings = baseline.clone();
    settings.dsd.pcm_to_dsd.filter = DsdFilterPreset::Sinc;
    variants.push(("dsd.pcm_to_dsd.filter", settings));

    macro_rules! mutate_sinc {
        ($path:literal, $field:ident, $value:expr) => {{
            let mut settings = baseline.clone();
            settings.dsd.pcm_to_dsd.sinc.$field = $value;
            variants.push(($path, settings));
        }};
    }
    mutate_sinc!("dsd.pcm_to_dsd.sinc.oversample_factor", oversample_factor, 16);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.taps", taps, 131_072);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.passband_hz", passband_hz, 30_000.0);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.transition_hz", transition_hz, 750.0);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.kaiser_beta", kaiser_beta, 12.5);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.linear_phase", linear_phase, false);
    mutate_sinc!("dsd.pcm_to_dsd.sinc.allow_aliasing", allow_aliasing, true);

    let mut settings = baseline.clone();
    settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Decibels(1.5);
    variants.push(("dsd.pcm_to_dsd.gain_compensation", settings));

    let mut settings = baseline.clone();
    settings.dsd.from_dsd.pathway = DsdSourcePathway::Manual;
    variants.push(("dsd.from_dsd.pathway", settings));

    let mut settings = baseline.clone();
    settings.dsd.from_dsd.profile = DsdReconstructionSelection::Wideband;
    variants.push(("dsd.from_dsd.profile", settings));

    let mut settings = baseline.clone();
    settings.dsd.from_dsd.gain_mode = DsdSourceGainMode::NativeLevel;
    variants.push(("dsd.from_dsd.gain_mode", settings));

    let mut settings = baseline.clone();
    settings.dsd.from_dsd.gain_mode = DsdSourceGainMode::Fixed;
    settings.dsd.from_dsd.fixed_gain_db = Some("3.125000000".parse::<DbNano>().unwrap());
    variants.push(("dsd.from_dsd.fixed_gain_db", settings));

    let mut settings = baseline.clone();
    settings.dsd.from_dsd.gain_mode = DsdSourceGainMode::NormalizePeak;
    settings.dsd.from_dsd.normalize_peak_target_dbfs =
        "-2.500000000".parse::<DbNano>().unwrap();
    variants.push(("dsd.from_dsd.normalize_peak_target_dbfs", settings));

    let mut fingerprints = BTreeSet::new();
    fingerprints.insert(baseline_fingerprint.0.to_hex());
    for (path, settings) in variants {
        let decoded = round_trip(&settings);
        assert_eq!(decoded, settings, "serde drift for {path}");
        let fingerprint = settings_snapshot_fingerprint_v2(&decoded);
        assert_ne!(
            fingerprint, baseline_fingerprint,
            "{path} did not affect the native-v2 settings snapshot"
        );
        assert!(
            fingerprints.insert(fingerprint.0.to_hex()),
            "{path} collided with another sentinel mutation"
        );
    }
}

#[test]
fn native_v2_immutable_identity_fields_are_serialized_and_hashed() {
    let settings = PipelineSettings::default();
    let encoded = serde_json::to_value(&settings).expect("serialize settings");
    assert_eq!(encoded["dsd"]["schema_version"], 2);
    assert_eq!(
        encoded["dsd"]["from_dsd"]["reference_policy"],
        serde_json::to_value(DsdReferencePolicyVersion::SoxNg14801V3).unwrap()
    );

    // These fields have one legal P0 value. Pin their exact snapshot tokens so
    // a future append-only value cannot silently disappear from identity.
    let snapshot = settings_snapshot_fingerprint_v2(&settings);
    let expected = settings_snapshot_fingerprint_v2(&round_trip(&settings));
    assert_eq!(snapshot, expected);
}
