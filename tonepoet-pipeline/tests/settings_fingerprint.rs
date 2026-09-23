//! Integration tests for deterministic Phase-2 pipeline settings fingerprints.

use std::collections::BTreeSet;

use tonepoet_pipeline::{
    settings_fingerprint, AudioFormat, BitDepthTarget, DbNano, DsdGeneralExportLevel,
    DsdGeneralReconstruction, DsdLowpassMethod, DsdSettings, PcmBitDepth, PipelineSettings,
    RateTarget, SampleGainPolicy, TruePeakScanTier, TruePeakScope,
    DSD_ALBUM_GAIN_FINGERPRINT_FIELD_PATHS, PCM_TRUE_PEAK_FINGERPRINT_FIELD_PATHS,
    SETTINGS_FINGERPRINT_FIELD_COUNT, SETTINGS_FINGERPRINT_FIELD_PATHS,
};

fn db(value: &str) -> DbNano {
    value.parse().expect("valid dB fixture")
}

fn guard(scope: TruePeakScope, scan: TruePeakScanTier, target: &str) -> SampleGainPolicy {
    SampleGainPolicy::TruePeakGuard {
        target_dbtp: db(target),
        scope,
        scan,
    }
}

fn normalize(scope: TruePeakScope, scan: TruePeakScanTier, target: &str) -> SampleGainPolicy {
    SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db(target),
        scope,
        scan,
    }
}

fn dsd_base() -> PipelineSettings {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Flac;
    settings.target_sample_rate = RateTarget::PcmHz(96_000);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    settings.dsd = DsdSettings::default();
    settings
}

#[test]
fn fingerprint_field_inventory_is_unique_and_matches_constant() {
    assert_eq!(
        SETTINGS_FINGERPRINT_FIELD_COUNT,
        SETTINGS_FINGERPRINT_FIELD_PATHS.len()
    );
    let unique: BTreeSet<_> = SETTINGS_FINGERPRINT_FIELD_PATHS.iter().copied().collect();
    assert_eq!(unique.len(), SETTINGS_FINGERPRINT_FIELD_PATHS.len());

    for retired in [
        "dsd.dsd_to_pcm_gain_mode",
        "dsd.dsd_to_pcm_auto_gain_margin_db",
        "dsd.from_dsd.automatic_gain_scope",
        "pcm_true_peak.enabled",
        "pcm_true_peak.allow_boost",
        "pcm_true_peak.fixed_gain_db",
    ] {
        assert!(
            !unique.contains(retired),
            "retired ambiguous authority leaked into current identity: {retired}"
        );
    }
}

#[test]
fn phase2_gain_identity_paths_are_explicit() {
    assert_eq!(
        DSD_ALBUM_GAIN_FINGERPRINT_FIELD_PATHS,
        &[
            "dsd.from_dsd.gain.mode",
            "dsd.from_dsd.gain.target_dbtp",
            "dsd.from_dsd.gain.scope",
            "dsd.from_dsd.gain.scan",
            "dsd.general_from_dsd.gain.mode",
            "dsd.general_from_dsd.gain.target_dbtp",
            "dsd.general_from_dsd.gain.scope",
            "dsd.general_from_dsd.gain.scan",
            "dsd.runtime_album_gain_db",
        ]
    );
    assert_eq!(
        PCM_TRUE_PEAK_FINGERPRINT_FIELD_PATHS,
        &[
            "pcm_true_peak.policy.mode",
            "pcm_true_peak.policy.target_dbtp",
            "pcm_true_peak.policy.scope",
            "pcm_true_peak.policy.scan",
            "pcm_true_peak.policy.gain_db",
            "pcm_true_peak.runtime_album_gain_db",
        ]
    );
}

#[test]
fn pcm_gain_modes_targets_scopes_and_tiers_are_distinct_identities() {
    let base = PipelineSettings::default();
    let off = settings_fingerprint(&base);

    let mut guard_track_fast = base.clone();
    guard_track_fast.pcm_true_peak.policy = guard(
        TruePeakScope::Track,
        TruePeakScanTier::Fast,
        "-0.100000000",
    );
    let guard_track_fast_fp = settings_fingerprint(&guard_track_fast);
    assert_ne!(off, guard_track_fast_fp);

    let mut normalized = guard_track_fast.clone();
    normalized.pcm_true_peak.policy = normalize(
        TruePeakScope::Track,
        TruePeakScanTier::Fast,
        "-0.100000000",
    );
    assert_ne!(
        guard_track_fast_fp,
        settings_fingerprint(&normalized),
        "Guard and Normalize must not collide when their numeric controls match"
    );

    let mut album = guard_track_fast.clone();
    album.pcm_true_peak.policy = album
        .pcm_true_peak
        .policy
        .with_scope(TruePeakScope::Album);
    assert_ne!(guard_track_fast_fp, settings_fingerprint(&album));

    let mut standard = guard_track_fast.clone();
    standard.pcm_true_peak.policy = standard
        .pcm_true_peak
        .policy
        .with_scan(TruePeakScanTier::Standard);
    assert_ne!(guard_track_fast_fp, settings_fingerprint(&standard));

    let mut target = guard_track_fast.clone();
    target.pcm_true_peak.policy = target
        .pcm_true_peak
        .policy
        .with_target(db("-0.750000000"));
    assert_ne!(guard_track_fast_fp, settings_fingerprint(&target));

    let mut fixed = base.clone();
    fixed.pcm_true_peak.policy = SampleGainPolicy::FixedGain {
        gain_db: db("3.250000000"),
    };
    let fixed_fp = settings_fingerprint(&fixed);
    assert_ne!(off, fixed_fp);
    fixed.pcm_true_peak.policy = SampleGainPolicy::FixedGain {
        gain_db: db("3.251000000"),
    };
    assert_ne!(fixed_fp, settings_fingerprint(&fixed));
}

#[test]
fn pcm_runtime_album_authority_is_fingerprinted_but_not_persisted() {
    let mut settings = PipelineSettings::default();
    settings.pcm_true_peak.policy = guard(
        TruePeakScope::Album,
        TruePeakScanTier::Standard,
        "-0.100000000",
    );
    let unbound = settings_fingerprint(&settings);
    settings
        .pcm_true_peak
        .set_runtime_album_gain_db(Some(db("-1.250000000")));
    assert_ne!(unbound, settings_fingerprint(&settings));

    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_value(&settings).expect("serialize settings");
        assert!(json.pointer("/pcm_true_peak/runtime_album_gain_db").is_none());
    }
}

#[test]
fn dsd_gain_modes_targets_scopes_and_tiers_are_distinct_identities() {
    let mut base = dsd_base();
    let off = settings_fingerprint(&base);

    base.dsd.set_gain_policy(guard(
        TruePeakScope::Track,
        TruePeakScanTier::Reference,
        "-0.100000000",
    ));
    let guard_track = settings_fingerprint(&base);
    assert_ne!(off, guard_track);

    let mut normalized = base.clone();
    normalized.dsd.set_gain_policy(normalize(
        TruePeakScope::Track,
        TruePeakScanTier::Reference,
        "-0.100000000",
    ));
    assert_ne!(guard_track, settings_fingerprint(&normalized));

    let mut album = base.clone();
    album.dsd.set_true_peak_scope(TruePeakScope::Album);
    assert_ne!(guard_track, settings_fingerprint(&album));

    let mut fast = base.clone();
    fast.dsd.set_true_peak_scan_tier(TruePeakScanTier::Fast);
    assert_ne!(guard_track, settings_fingerprint(&fast));

    let mut target = base.clone();
    target.dsd.set_gain_policy(
        target
            .dsd
            .gain_policy()
            .with_target(db("-0.500000000")),
    );
    assert_ne!(guard_track, settings_fingerprint(&target));

    let mut fixed = dsd_base();
    fixed.dsd.set_gain_policy(SampleGainPolicy::FixedGain {
        gain_db: db("-2.000000000"),
    });
    assert_ne!(off, settings_fingerprint(&fixed));
}

#[test]
fn dsd_directional_reconstruction_export_and_sinc_controls_are_fingerprinted() {
    let base = dsd_base();
    let baseline = settings_fingerprint(&base);

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
    assert_ne!(baseline, settings_fingerprint(&changed));

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::NominalCompensated;
    assert_ne!(baseline, settings_fingerprint(&changed));

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::NativeWithOffset {
        offset_db: db("1.250000000"),
    };
    assert_ne!(baseline, settings_fingerprint(&changed));

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;
    assert_ne!(baseline, settings_fingerprint(&changed));

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.sinc.taps += 2;
    assert_ne!(baseline, settings_fingerprint(&changed));

    let mut changed = base.clone();
    changed.dsd.general_from_dsd.sinc.passband_hz += 100.0;
    assert_ne!(baseline, settings_fingerprint(&changed));
}

#[test]
fn dsd_runtime_album_authority_is_fingerprinted_at_nanodecibel_precision() {
    let mut settings = dsd_base();
    settings.dsd.set_gain_policy(guard(
        TruePeakScope::Album,
        TruePeakScanTier::Reference,
        "-0.100000000",
    ));
    let unbound = settings_fingerprint(&settings);
    settings
        .dsd
        .set_runtime_album_gain_db(Some(db("-2.125000000")));
    let bound = settings_fingerprint(&settings);
    assert_ne!(unbound, bound);
    settings
        .dsd
        .set_runtime_album_gain_db(Some(db("-2.124999999")));
    assert_ne!(bound, settings_fingerprint(&settings));
}

#[test]
fn explicit_reference_and_general_pathways_have_distinct_settings_identity() {
    let general = dsd_base();
    let mut reference = general.clone();
    reference.dsd = DsdSettings::reference();
    assert_ne!(settings_fingerprint(&general), settings_fingerprint(&reference));
}

#[test]
fn native_replaygain_mode_is_part_of_settings_identity() {
    let base = PipelineSettings::default();
    let mut both = base.clone();
    both.replay_gain.mode = Some(tonepoet_pipeline::ReplayGainMode::Both);
    assert_ne!(settings_fingerprint(&base), settings_fingerprint(&both));
    assert_eq!(both.replay_gain.logical_mode(), Some(tonepoet_pipeline::ReplayGainMode::Both));
}

#[test]
fn fingerprint_is_deterministic_for_identical_strict_settings() {
    let mut settings = dsd_base();
    settings.dsd.set_gain_policy(normalize(
        TruePeakScope::Album,
        TruePeakScanTier::Standard,
        "-0.250000000",
    ));
    settings.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::NativeWithOffset {
        offset_db: db("0.500000000"),
    };
    assert_eq!(settings_fingerprint(&settings), settings_fingerprint(&settings.clone()));
}
