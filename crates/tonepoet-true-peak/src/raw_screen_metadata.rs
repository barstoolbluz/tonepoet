//! Generated native-input screening metadata for Fast066V2 / HQ1024V1.
//!
//! The authority is the exact derivation in `qualification/generate_raw_screen_metadata.py`.
//! Frozen coefficient source SHA-256:
//! 7070c2e9abc255062dd30aaa516c0827d969d238759e14a61c5d1da94a67de9d

/// Upper bound on the second-antiderivative residual coefficient norm.
pub(crate) const RAW_A_UPPER: f64 = f64::from_bits(0x3ff0_2862_ce0a_81a1);
/// Upper bound on the two affine-defect coefficients.
pub(crate) const RAW_B_UPPER: f64 = f64::from_bits(0x3cce_9970_cbf1_2e12);

/// Downward-rounded linear ratio corresponding to 0.01 dB.
///
/// This is deliberately private: the public Fast tier has no interval objective.
pub(crate) const FAST_ACCEPT_RATIO_DOWN: f64 = f64::from_bits(0x3ff0_04b7_e9b5_ce5c);
