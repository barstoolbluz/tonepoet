#![allow(unsafe_code)]

//! Narrow Rust wrapper around the in-process libavcodec MLP stereo decoder.
//!
//! The C boundary writes the FFmpeg 7.1 MLP decoder private `downmix_layout`
//! field after validating the public AVOption offset. Rust does not inspect
//! FFmpeg private decoder structs and only passes paths plus value structs
//! across the FFI seam.

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::{c_char, c_int, c_uint};
use std::path::Path;

const TEXT_CAP: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy)]
struct TonepoetNativeMlpDecoderInfoRaw {
    decoder_available: c_int,
    downmix_option_available: c_int,
    private_downmix_layout_available: c_int,
    private_downmix_layout_set: c_int,
    downmix_option_offset: c_int,
    private_downmix_layout_offset: c_int,
    avcodec_version: c_uint,
    avcodec_version_text: [c_char; TEXT_CAP],
    avcodec_configuration: [c_char; TEXT_CAP],
    error: [c_char; TEXT_CAP],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TonepoetNativeMlpDecodeResultRaw {
    channels: c_int,
    sample_rate: c_int,
    samples_per_channel: u64,
    data_bytes: u64,
    avcodec_version: c_uint,
    private_downmix_layout_set: c_int,
    downmix_option_offset: c_int,
    private_downmix_layout_offset: c_int,
    channel_layout: [c_char; TEXT_CAP],
    error: [c_char; TEXT_CAP],
}

unsafe extern "C" {
    fn tonepoet_native_mlp_decoder_info(out: *mut TonepoetNativeMlpDecoderInfoRaw) -> c_int;

    fn tonepoet_native_mlp_decode_stereo_s32le_wav(
        input_path: *const c_char,
        output_path: *const c_char,
        out: *mut TonepoetNativeMlpDecodeResultRaw,
    ) -> c_int;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeMlpDecoderInfo {
    pub(crate) decoder_available: bool,
    pub(crate) downmix_option_available: bool,
    pub(crate) private_downmix_layout_available: bool,
    pub(crate) private_downmix_layout_set: bool,
    pub(crate) downmix_option_offset: Option<i32>,
    pub(crate) private_downmix_layout_offset: Option<i32>,
    pub(crate) avcodec_version: Option<String>,
    pub(crate) avcodec_configuration: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeMlpDecodeResult {
    pub(crate) channels: u32,
    pub(crate) sample_rate: u32,
    pub(crate) samples_per_channel: u64,
    pub(crate) data_bytes: u64,
    pub(crate) private_downmix_layout_set: bool,
    pub(crate) downmix_option_offset: Option<i32>,
    pub(crate) private_downmix_layout_offset: Option<i32>,
    pub(crate) channel_layout: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeMlpError {
    InvalidPath { path: String },
    #[allow(dead_code)]
    ProbeFailed(String),
    DecodeFailed(String),
    NonStereoOutput { channels: u32, layout: Option<String> },
}

impl fmt::Display for NativeMlpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => write!(f, "path cannot be passed to native MLP decoder: {path}"),
            Self::ProbeFailed(message) => write!(f, "native MLP decoder probe failed: {message}"),
            Self::DecodeFailed(message) => write!(f, "native MLP decode failed: {message}"),
            Self::NonStereoOutput { channels, layout } => write!(
                f,
                "native MLP decode emitted {channels} channels with layout {}",
                layout.as_deref().unwrap_or("unknown")
            ),
        }
    }
}

impl std::error::Error for NativeMlpError {}

pub(crate) fn decoder_info() -> NativeMlpDecoderInfo {
    let mut raw = TonepoetNativeMlpDecoderInfoRaw::zeroed();
    let rc = unsafe { tonepoet_native_mlp_decoder_info(&mut raw) };
    if rc != 0 {
        return NativeMlpDecoderInfo {
            decoder_available: false,
            downmix_option_available: false,
            private_downmix_layout_available: false,
            private_downmix_layout_set: false,
            downmix_option_offset: None,
            private_downmix_layout_offset: None,
            avcodec_version: None,
            avcodec_configuration: None,
            error: Some(format!("native decoder info call returned {rc}")),
        };
    }
    NativeMlpDecoderInfo {
        decoder_available: raw.decoder_available != 0,
        downmix_option_available: raw.downmix_option_available != 0,
        private_downmix_layout_available: raw.private_downmix_layout_available != 0,
        private_downmix_layout_set: raw.private_downmix_layout_set != 0,
        downmix_option_offset: nonnegative_c_int(raw.downmix_option_offset),
        private_downmix_layout_offset: nonnegative_c_int(raw.private_downmix_layout_offset),
        avcodec_version: nonempty_c_char_buf(&raw.avcodec_version_text),
        avcodec_configuration: nonempty_c_char_buf(&raw.avcodec_configuration),
        error: nonempty_c_char_buf(&raw.error),
    }
}

pub(crate) fn decode_authored_stereo_to_wav(
    mlp_path: &Path,
    wav_path: &Path,
) -> Result<NativeMlpDecodeResult, NativeMlpError> {
    let input = path_to_cstring(mlp_path)?;
    let output = path_to_cstring(wav_path)?;
    let mut raw = TonepoetNativeMlpDecodeResultRaw::zeroed();
    let rc = unsafe {
        tonepoet_native_mlp_decode_stereo_s32le_wav(input.as_ptr(), output.as_ptr(), &mut raw)
    };
    if rc != 0 {
        let message = nonempty_c_char_buf(&raw.error)
            .unwrap_or_else(|| format!("native decoder returned {rc}"));
        return Err(NativeMlpError::DecodeFailed(message));
    }
    let channels = u32::try_from(raw.channels).unwrap_or(0);
    let layout = nonempty_c_char_buf(&raw.channel_layout);
    if channels != 2 {
        return Err(NativeMlpError::NonStereoOutput { channels, layout });
    }
    Ok(NativeMlpDecodeResult {
        channels,
        sample_rate: u32::try_from(raw.sample_rate).unwrap_or(0),
        samples_per_channel: raw.samples_per_channel,
        data_bytes: raw.data_bytes,
        private_downmix_layout_set: raw.private_downmix_layout_set != 0,
        downmix_option_offset: nonnegative_c_int(raw.downmix_option_offset),
        private_downmix_layout_offset: nonnegative_c_int(raw.private_downmix_layout_offset),
        channel_layout: layout,
    })
}

fn path_to_cstring(path: &Path) -> Result<CString, NativeMlpError> {
    let path_string = path.to_string_lossy().into_owned();
    CString::new(path_string.clone()).map_err(|_| NativeMlpError::InvalidPath { path: path_string })
}

fn nonnegative_c_int(value: c_int) -> Option<i32> {
    (value >= 0).then_some(value)
}

fn nonempty_c_char_buf(buf: &[c_char]) -> Option<String> {
    let ptr = buf.as_ptr();
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    let trimmed = text.trim_matches(char::from(0)).trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

impl TonepoetNativeMlpDecoderInfoRaw {
    fn zeroed() -> Self {
        Self {
            decoder_available: 0,
            downmix_option_available: 0,
            private_downmix_layout_available: 0,
            private_downmix_layout_set: 0,
            downmix_option_offset: -1,
            private_downmix_layout_offset: -1,
            avcodec_version: 0,
            avcodec_version_text: [0; TEXT_CAP],
            avcodec_configuration: [0; TEXT_CAP],
            error: [0; TEXT_CAP],
        }
    }
}

impl TonepoetNativeMlpDecodeResultRaw {
    fn zeroed() -> Self {
        Self {
            channels: 0,
            sample_rate: 0,
            samples_per_channel: 0,
            data_bytes: 0,
            avcodec_version: 0,
            private_downmix_layout_set: 0,
            downmix_option_offset: -1,
            private_downmix_layout_offset: -1,
            channel_layout: [0; TEXT_CAP],
            error: [0; TEXT_CAP],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_char_buffer_conversion_returns_none_for_empty_buffer() {
        let buf = [0 as c_char; TEXT_CAP];
        assert_eq!(nonempty_c_char_buf(&buf), None);
    }

    #[test]
    fn invalid_path_with_nul_is_rejected_before_ffi() {
        let err = CString::new("bad\0path").expect_err("test input contains nul");
        assert!(err.nul_position() > 0);
    }

    #[test]
    fn non_stereo_result_is_an_error() {
        let err = NativeMlpError::NonStereoOutput {
            channels: 6,
            layout: Some("5.1".to_string()),
        };
        assert!(err.to_string().contains("6 channels"));
    }
}
