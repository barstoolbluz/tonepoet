//! WAV header writing and readback validation for realized Blu-ray LPCM.
//!
//! The realizer writes to a temporary file, rewrites the RIFF header with the
//! final data length, validates the emitted WAV metadata by reading it back, and
//! only then publishes the temporary file atomically.

use std::fs;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use super::bluray_pts::PTS_CLOCK_HZ;
use super::errors::ConvertError;

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAV_PCM_GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LpcmWavFormat {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) container_bits_per_sample: u16,
    pub(crate) valid_bits_per_sample: u16,
    pub(crate) channel_mask: Option<u32>,
    pub(crate) format_extensible: bool,
}

impl LpcmWavFormat {
    pub(crate) fn new(
        sample_rate: u32,
        channels: u8,
        container_bits_per_sample: u16,
        valid_bits_per_sample: u16,
        channel_mask: Option<u32>,
    ) -> Result<Self, ConvertError> {
        if !matches!(container_bits_per_sample, 16 | 24) {
            return Err(ConvertError::TrackValidation(format!(
                "unsupported Blu-ray LPCM container bit depth {container_bits_per_sample}; expected 16 or 24"
            )));
        }
        if !matches!(valid_bits_per_sample, 16 | 20 | 24) || valid_bits_per_sample > container_bits_per_sample {
            return Err(ConvertError::TrackValidation(format!(
                "unsupported Blu-ray LPCM valid bit depth {valid_bits_per_sample} in {container_bits_per_sample}-bit container"
            )));
        }
        let format_extensible = valid_bits_per_sample != container_bits_per_sample
            || channels > 2
            || channel_mask.is_some();
        Ok(Self {
            sample_rate,
            channels: u16::from(channels),
            container_bits_per_sample,
            valid_bits_per_sample,
            channel_mask,
            format_extensible,
        })
    }

    pub(crate) const fn bytes_per_sample(self) -> u16 {
        self.container_bits_per_sample / 8
    }

    pub(crate) fn block_align(self) -> Result<u16, ConvertError> {
        self.channels
            .checked_mul(self.bytes_per_sample())
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM WAV block-align overflow".to_string()))
    }

    pub(crate) fn byte_rate(self) -> Result<u32, ConvertError> {
        self.sample_rate
            .checked_mul(u32::from(self.block_align()?))
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM WAV byte-rate overflow".to_string()))
    }

    const fn fmt_chunk_size(self) -> u32 {
        if self.format_extensible {
            40
        } else {
            16
        }
    }
}

pub(crate) fn write_wav_header(
    output: &mut File,
    format: LpcmWavFormat,
    data_len: u64,
) -> Result<(), ConvertError> {
    let riff_size = checked_riff_size(format, data_len)?;
    let data_size = u32::try_from(data_len).map_err(|_| {
        ConvertError::Realize(format!(
            "Blu-ray LPCM WAV data is too large for classic RIFF/WAVE: {data_len} bytes"
        ))
    })?;

    output.write_all(b"RIFF").map_err(wav_write_error)?;
    output.write_all(&riff_size.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(b"WAVE").map_err(wav_write_error)?;
    output.write_all(b"fmt ").map_err(wav_write_error)?;
    output
        .write_all(&format.fmt_chunk_size().to_le_bytes())
        .map_err(wav_write_error)?;

    if format.format_extensible {
        output.write_all(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes()).map_err(wav_write_error)?;
    } else {
        output.write_all(&WAVE_FORMAT_PCM.to_le_bytes()).map_err(wav_write_error)?;
    }
    output.write_all(&format.channels.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.sample_rate.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.byte_rate()?.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.block_align()?.to_le_bytes()).map_err(wav_write_error)?;
    output
        .write_all(&format.container_bits_per_sample.to_le_bytes())
        .map_err(wav_write_error)?;

    if format.format_extensible {
        output.write_all(&22u16.to_le_bytes()).map_err(wav_write_error)?;
        output
            .write_all(&format.valid_bits_per_sample.to_le_bytes())
            .map_err(wav_write_error)?;
        output
            .write_all(&format.channel_mask.unwrap_or(0).to_le_bytes())
            .map_err(wav_write_error)?;
        output
            .write_all(&0x0001u16.to_le_bytes())
            .map_err(wav_write_error)?;
        output.write_all(&WAV_PCM_GUID_TAIL).map_err(wav_write_error)?;
    }

    output.write_all(b"data").map_err(wav_write_error)?;
    output.write_all(&data_size.to_le_bytes()).map_err(wav_write_error)
}

pub(crate) fn rewrite_wav_header(
    output: &mut File,
    format: LpcmWavFormat,
    data_len: u64,
) -> Result<(), ConvertError> {
    output.seek(SeekFrom::Start(0)).map_err(|err| {
        ConvertError::Realize(format!("failed to seek Blu-ray LPCM WAV header: {err}"))
    })?;
    write_wav_header(output, format, data_len)
}

fn checked_riff_size(format: LpcmWavFormat, data_len: u64) -> Result<u32, ConvertError> {
    let riff_size = 20u64
        .checked_add(u64::from(format.fmt_chunk_size()))
        .and_then(|size| size.checked_add(data_len))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM WAV RIFF size overflow".to_string()))?;
    u32::try_from(riff_size).map_err(|_| {
        ConvertError::Realize(format!(
            "Blu-ray LPCM WAV is too large for classic RIFF/WAVE: {data_len} bytes of audio data"
        ))
    })
}


pub(crate) fn expected_pcm_data_bytes(duration_pts_90k: u64, format: LpcmWavFormat) -> Result<u64, ConvertError> {
    let sample_frames = u128::from(duration_pts_90k)
        .checked_mul(u128::from(format.sample_rate))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM expected sample count overflow".to_string()))?
        / u128::from(PTS_CLOCK_HZ);
    let bytes = sample_frames
        .checked_mul(u128::from(format.block_align()?))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM expected byte count overflow".to_string()))?;
    u64::try_from(bytes).map_err(|_| {
        ConvertError::Realize("Blu-ray LPCM expected byte count exceeds u64".to_string())
    })
}

fn wav_write_error(err: io::Error) -> ConvertError {
    ConvertError::Realize(format!("failed to write Blu-ray LPCM WAV header: {err}"))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExpectedAudio {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u8,
    pub(crate) container_bits: u16,
    pub(crate) valid_bits: u16,
    pub(crate) channel_mask: Option<u32>,
    pub(crate) chapter_duration_pts_90k: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WavInfo {
    pub(crate) format_tag: u16,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) byte_rate: u32,
    pub(crate) block_align: u16,
    pub(crate) bits_per_sample: u16,
    pub(crate) valid_bits_per_sample: Option<u16>,
    pub(crate) channel_mask: Option<u32>,
    pub(crate) data_size: u64,
    pub(crate) riff_size: u64,
}

pub(crate) fn validate_bluray_lpcm_wav(path: &Path, expected: &ExpectedAudio) -> Result<(), String> {
    let wav = read_wav_info(path)?;

    require(wav.data_size > 0, "empty WAV data chunk")?;
    require(
        wav.sample_rate == expected.sample_rate,
        format!(
            "sample rate mismatch: expected {}, WAV says {}",
            expected.sample_rate, wav.sample_rate
        ),
    )?;
    require(
        wav.channels == u16::from(expected.channels),
        format!(
            "channel count mismatch: expected {}, WAV says {}",
            expected.channels, wav.channels
        ),
    )?;
    require(
        wav.bits_per_sample == expected.container_bits,
        format!(
            "container bit depth mismatch: expected {}, WAV says {}",
            expected.container_bits, wav.bits_per_sample
        ),
    )?;

    let extensible_required = expected.valid_bits != expected.container_bits
        || expected.channels > 2
        || expected.channel_mask.is_some();
    if extensible_required {
        require(
            wav.format_tag == WAVE_FORMAT_EXTENSIBLE,
            "Blu-ray LPCM output requiring valid bits, more than two channels, or a channel mask must use WAVE_FORMAT_EXTENSIBLE",
        )?;
    }

    if expected.valid_bits != expected.container_bits || wav.format_tag == WAVE_FORMAT_EXTENSIBLE {
        require(
            wav.valid_bits_per_sample == Some(expected.valid_bits),
            format!(
                "valid bit depth mismatch: expected {}, WAV says {:?}",
                expected.valid_bits, wav.valid_bits_per_sample
            ),
        )?;
    }
    if let Some(mask) = expected.channel_mask {
        require(
            wav.channel_mask == Some(mask),
            format!(
                "channel mask mismatch: expected 0x{mask:08x}, WAV says {:?}",
                wav.channel_mask
            ),
        )?;
    }

    require(wav.bits_per_sample % 8 == 0, "bits per sample is not byte-aligned")?;
    let expected_block_align = wav
        .channels
        .checked_mul(wav.bits_per_sample / 8)
        .ok_or_else(|| "block align calculation overflow".to_string())?;
    require(
        wav.block_align == expected_block_align,
        format!(
            "block align mismatch: expected {}, WAV says {}",
            expected_block_align, wav.block_align
        ),
    )?;
    let expected_byte_rate = wav
        .sample_rate
        .checked_mul(u32::from(wav.block_align))
        .ok_or_else(|| "byte rate calculation overflow".to_string())?;
    require(
        wav.byte_rate == expected_byte_rate,
        format!(
            "byte rate mismatch: expected {}, WAV says {}",
            expected_byte_rate, wav.byte_rate
        ),
    )?;
    require(
        wav.data_size % u64::from(wav.block_align) == 0,
        "data chunk is not frame-aligned",
    )?;

    if let Some(duration_pts_90k) = expected.chapter_duration_pts_90k {
        let expected_samples = pts90_to_samples_for_validation(duration_pts_90k, expected.sample_rate)?;
        let expected_bytes = expected_samples
            .checked_mul(u64::from(expected_block_align))
            .ok_or_else(|| "expected byte count overflow".to_string())?;
        let tolerance_bytes = u64::from(expected_block_align)
            .checked_mul(u64::from(expected.sample_rate))
            .ok_or_else(|| "WAV size tolerance overflow".to_string())?
            / 10;
        let delta = wav.data_size.abs_diff(expected_bytes);
        require(
            delta <= tolerance_bytes,
            format!(
                "data size {} differs from chapter-duration estimate {} by {} byte(s), exceeding 100 ms tolerance {}",
                wav.data_size, expected_bytes, delta, tolerance_bytes
            ),
        )?;
    }

    Ok(())
}

pub(crate) fn read_wav_info(path: &Path) -> Result<WavInfo, String> {
    let bytes = fs::read(path).map_err(|err| format!("failed to read WAV '{}': {err}", path.display()))?;
    if bytes.len() < 12 {
        return Err("truncated RIFF header".to_string());
    }
    require(&bytes[0..4] == b"RIFF", "missing RIFF signature")?;
    require(&bytes[8..12] == b"WAVE", "missing WAVE signature")?;

    let riff_size_field = read_u32_le(&bytes, 4, "RIFF size")?;
    let riff_total_size = u64::from(riff_size_field)
        .checked_add(8)
        .ok_or_else(|| "RIFF size overflow".to_string())?;
    require(
        riff_total_size == bytes.len() as u64,
        format!(
            "RIFF size mismatch: header declares {} total byte(s), file has {} byte(s)",
            riff_total_size,
            bytes.len()
        ),
    )?;

    let mut offset = 12usize;
    let riff_end = usize::try_from(riff_total_size).map_err(|_| "RIFF size exceeds usize".to_string())?;
    let mut fmt: Option<WavInfo> = None;
    let mut data_size: Option<u64> = None;

    while offset < riff_end {
        if riff_end - offset < 8 {
            return Err("truncated RIFF chunk header".to_string());
        }
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = read_u32_le(&bytes, offset + 4, "chunk size")? as usize;
        let data_start = offset
            .checked_add(8)
            .ok_or_else(|| "RIFF chunk offset overflow".to_string())?;
        let data_end = data_start
            .checked_add(chunk_size)
            .ok_or_else(|| "RIFF chunk size overflow".to_string())?;
        if data_end > riff_end {
            return Err(format!(
                "truncated RIFF chunk '{}': declares {} byte(s) past file end",
                chunk_id_label(chunk_id), chunk_size
            ));
        }

        if chunk_id == b"fmt " {
            if fmt.is_some() {
                return Err("multiple fmt chunks are not supported".to_string());
            }
            if chunk_size < 16 {
                return Err("truncated fmt chunk".to_string());
            }
            let format_tag = read_u16_le(&bytes, data_start, "format tag")?;
            let channels = read_u16_le(&bytes, data_start + 2, "channels")?;
            let sample_rate = read_u32_le(&bytes, data_start + 4, "sample rate")?;
            let byte_rate = read_u32_le(&bytes, data_start + 8, "byte rate")?;
            let block_align = read_u16_le(&bytes, data_start + 12, "block align")?;
            let bits_per_sample = read_u16_le(&bytes, data_start + 14, "bits per sample")?;
            let (valid_bits_per_sample, channel_mask) = if format_tag == WAVE_FORMAT_EXTENSIBLE {
                if chunk_size < 40 {
                    return Err("truncated WAVE_FORMAT_EXTENSIBLE fmt chunk".to_string());
                }
                let cb_size = read_u16_le(&bytes, data_start + 16, "extensible cbSize")?;
                if cb_size < 22 {
                    return Err(format!(
                        "WAVE_FORMAT_EXTENSIBLE fmt chunk has cbSize {cb_size}, expected at least 22"
                    ));
                }
                let valid_bits = read_u16_le(&bytes, data_start + 18, "valid bits per sample")?;
                let mask = read_u32_le(&bytes, data_start + 20, "channel mask")?;
                let subformat_tag = read_u16_le(&bytes, data_start + 24, "subformat tag")?;
                if subformat_tag != WAVE_FORMAT_PCM || &bytes[data_start + 26..data_start + 40] != WAV_PCM_GUID_TAIL.as_slice() {
                    return Err("WAVE_FORMAT_EXTENSIBLE subformat is not PCM".to_string());
                }
                (Some(valid_bits), Some(mask))
            } else {
                (None, None)
            };
            fmt = Some(WavInfo {
                format_tag,
                channels,
                sample_rate,
                byte_rate,
                block_align,
                bits_per_sample,
                valid_bits_per_sample,
                channel_mask,
                data_size: 0,
                riff_size: u64::from(riff_size_field),
            });
        } else if chunk_id == b"data" {
            if data_size.is_some() {
                return Err("multiple data chunks are not supported".to_string());
            }
            data_size = Some(chunk_size as u64);
        }

        offset = data_end
            .checked_add(chunk_size & 1)
            .ok_or_else(|| "RIFF chunk padding overflow".to_string())?;
    }

    let mut info = fmt.ok_or_else(|| "missing fmt chunk".to_string())?;
    info.data_size = data_size.ok_or_else(|| "missing data chunk".to_string())?;
    Ok(info)
}

fn read_u16_le(bytes: &[u8], offset: usize, what: &str) -> Result<u16, String> {
    let Some(end) = offset.checked_add(2) else {
        return Err(format!("truncated {what}"));
    };
    if end > bytes.len() {
        return Err(format!("truncated {what}"));
    }
    Ok(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize, what: &str) -> Result<u32, String> {
    let Some(end) = offset.checked_add(4) else {
        return Err(format!("truncated {what}"));
    };
    if end > bytes.len() {
        return Err(format!("truncated {what}"));
    }
    Ok(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn chunk_id_label(id: &[u8]) -> String {
    id.iter()
        .map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { char::from(*byte) } else { '?' })
        .collect()
}

fn pts90_to_samples_for_validation(duration_pts_90k: u64, sample_rate: u32) -> Result<u64, String> {
    let samples = u128::from(duration_pts_90k)
        .checked_mul(u128::from(sample_rate))
        .ok_or_else(|| "chapter-duration sample estimate overflow".to_string())?
        / u128::from(PTS_CLOCK_HZ);
    u64::try_from(samples).map_err(|_| "chapter-duration sample estimate exceeds u64".to_string())
}

fn require(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp_output_file(test_name: &str) -> (std::path::PathBuf, File) {
        let dir = std::env::temp_dir().join(format!(
            "bluray-wav-test-{test_name}-{}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");
        let file = File::create(&path).unwrap();
        (path, file)
    }

    fn write_test_wav(test_name: &str, format: LpcmWavFormat, data: &[u8]) -> std::path::PathBuf {
        let (path, mut file) = temp_output_file(test_name);
        write_wav_header(&mut file, format, data.len() as u64).unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
        path
    }

    fn expected_audio(
        sample_rate: u32,
        channels: u8,
        container_bits: u16,
        valid_bits: u16,
        channel_mask: Option<u32>,
    ) -> ExpectedAudio {
        ExpectedAudio {
            sample_rate,
            channels,
            container_bits,
            valid_bits,
            channel_mask,
            chapter_duration_pts_90k: None,
        }
    }

    #[test]
    fn wav_validation_accepts_standard_pcm_header() {
        let format = LpcmWavFormat::new(48_000, 2, 16, 16, None).unwrap();
        let path = write_test_wav("wav-standard", format, &[0, 0, 0, 0]);
        let info = read_wav_info(&path).unwrap();
        assert_eq!(info.format_tag, WAVE_FORMAT_PCM);
        validate_bluray_lpcm_wav(&path, &expected_audio(48_000, 2, 16, 16, None)).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_accepts_extensible_5_1_header() {
        let mask = 0x0000_060f;
        let format = LpcmWavFormat::new(48_000, 6, 24, 24, Some(mask)).unwrap();
        let path = write_test_wav("wav-extensible-5-1", format, &[0; 18]);
        let info = read_wav_info(&path).unwrap();
        assert_eq!(info.format_tag, WAVE_FORMAT_EXTENSIBLE);
        assert_eq!(info.channel_mask, Some(mask));
        validate_bluray_lpcm_wav(&path, &expected_audio(48_000, 6, 24, 24, Some(mask))).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_accepts_20_in_24_extensible_header() {
        let mask = 0x0000_0003;
        let format = LpcmWavFormat::new(96_000, 2, 24, 20, Some(mask)).unwrap();
        let path = write_test_wav("wav-20-in-24", format, &[0; 6]);
        let info = read_wav_info(&path).unwrap();
        assert_eq!(info.format_tag, WAVE_FORMAT_EXTENSIBLE);
        assert_eq!(info.valid_bits_per_sample, Some(20));
        validate_bluray_lpcm_wav(&path, &expected_audio(96_000, 2, 24, 20, Some(mask))).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_rejects_wrong_sample_rate() {
        let format = LpcmWavFormat::new(48_000, 2, 16, 16, None).unwrap();
        let path = write_test_wav("wav-wrong-rate", format, &[0, 0, 0, 0]);
        let err = validate_bluray_lpcm_wav(&path, &expected_audio(96_000, 2, 16, 16, None)).unwrap_err();
        assert!(err.contains("sample rate mismatch"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_rejects_wrong_channel_count() {
        let format = LpcmWavFormat::new(48_000, 2, 16, 16, None).unwrap();
        let path = write_test_wav("wav-wrong-channels", format, &[0, 0, 0, 0]);
        let err = validate_bluray_lpcm_wav(&path, &expected_audio(48_000, 6, 16, 16, None)).unwrap_err();
        assert!(err.contains("channel count mismatch"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_rejects_unaligned_data_chunk() {
        let format = LpcmWavFormat::new(48_000, 2, 16, 16, None).unwrap();
        let path = write_test_wav("wav-unaligned", format, &[0, 0, 0]);
        let err = validate_bluray_lpcm_wav(&path, &expected_audio(48_000, 2, 16, 16, None)).unwrap_err();
        assert!(err.contains("data chunk is not frame-aligned"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wav_validation_rejects_truncated_riff() {
        let (path, mut file) = temp_output_file("wav-truncated-riff");
        file.write_all(b"RIFF\x20\x00\x00\x00WAVEfmt ").unwrap();
        file.flush().unwrap();
        let err = validate_bluray_lpcm_wav(&path, &expected_audio(48_000, 2, 16, 16, None)).unwrap_err();
        assert!(err.contains("RIFF size mismatch") || err.contains("truncated"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
