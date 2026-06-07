#![forbid(unsafe_code)]

use crate::tui::dvda::error::{DvdaError, Result};

pub fn require_len(data: &[u8], needed: usize, context: impl Into<String>) -> Result<()> {
    if data.len() < needed {
        Err(DvdaError::ShortRead { context: context.into(), needed, available: data.len() })
    } else {
        Ok(())
    }
}

pub fn slice<'a>(data: &'a [u8], offset: usize, len: usize, context: impl Into<String>) -> Result<&'a [u8]> {
    let context = context.into();
    let end = offset
        .checked_add(len)
        .ok_or_else(|| DvdaError::parse(&context, "offset overflow"))?;
    if end > data.len() {
        return Err(DvdaError::bounds(context, offset, len, data.len()));
    }
    Ok(&data[offset..end])
}

pub fn u8_at(data: &[u8], offset: usize, context: impl Into<String>) -> Result<u8> {
    Ok(slice(data, offset, 1, context)?[0])
}

pub fn be_u16(data: &[u8], offset: usize, context: impl Into<String>) -> Result<u16> {
    let bytes = slice(data, offset, 2, context)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub fn be_u32(data: &[u8], offset: usize, context: impl Into<String>) -> Result<u32> {
    let bytes = slice(data, offset, 4, context)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn be_u64(data: &[u8], offset: usize, context: impl Into<String>) -> Result<u64> {
    let bytes = slice(data, offset, 8, context)?;
    Ok(u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub fn ascii_trim_nul(data: &[u8]) -> String {
    let first_nul = data.iter().position(|b| *b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..first_nul]).trim().to_string()
}

pub fn identifier(data: &[u8], offset: usize, len: usize, context: impl Into<String>) -> Result<String> {
    Ok(String::from_utf8_lossy(slice(data, offset, len, context)?).to_string())
}

pub fn sector_to_offset(sector: u32) -> Result<usize> {
    (sector as usize)
        .checked_mul(crate::tui::dvda::model::DVD_BLOCK_SIZE as usize)
        .ok_or_else(|| DvdaError::parse("sector pointer", "sector-to-byte offset overflow"))
}
