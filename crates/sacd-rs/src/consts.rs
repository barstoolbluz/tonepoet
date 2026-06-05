//! ScarletBook constants shared by SACD extraction code.

/// SACD timecode rate.
pub const FRAMES_PER_SECOND: u32 = 75;
pub const SECONDS_PER_MINUTE: u32 = 60;
pub const FRAMES_PER_MINUTE: u32 = SECONDS_PER_MINUTE * FRAMES_PER_SECOND;

/// DSD64 sample rate used by SACD.
pub const DSD64_SAMPLE_RATE: u32 = 2_822_400;

/// Eight-byte ScarletBook structure signatures.
pub const MASTER_TOC_SIGNATURE: &[u8; 8] = b"SACDMTOC";
pub const MASTER_TEXT_SIGNATURE: &[u8; 8] = b"SACDText";
pub const AREA_TOC_SIGNATURE_STEREO: &[u8; 8] = b"TWOCHTOC";
pub const AREA_TOC_SIGNATURE_MCH: &[u8; 8] = b"MULCHTOC";
pub const AREA_TRACK_TEXT_SIGNATURE: &[u8; 8] = b"SACDTTxt";
pub const AREA_TRACK_LIST_1_SIGNATURE: &[u8; 8] = b"SACDTRL1";
pub const AREA_TRACK_LIST_2_SIGNATURE: &[u8; 8] = b"SACDTRL2";
pub const AREA_ISRC_GENRE_SIGNATURE: &[u8; 8] = b"SACD_IGL";
