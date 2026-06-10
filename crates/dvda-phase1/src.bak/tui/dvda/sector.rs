#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::model::{AobFileEntry, DVD_BLOCK_SIZE, MAX_AOB_PARTS, MISSING_AOB_VIRTUAL_BYTES};
use crate::tui::dvda::volume::DvdaVolume;

pub fn build_aob_inventory<V: DvdaVolume + ?Sized>(volume: &V, title_set_nr: u8) -> Result<Vec<AobFileEntry>> {
    let mut out = Vec::with_capacity(MAX_AOB_PARTS as usize);
    let mut block_first: u32 = 0;
    for part in 1..=MAX_AOB_PARTS {
        let file_name = format!("ATS_{title_set_nr:02}_{part}.AOB");
        let len = volume.file_len(&file_name)?.unwrap_or(0);
        let exists = len > 0;
        let effective_len = if exists { len } else { MISSING_AOB_VIRTUAL_BYTES };
        let blocks = ceil_div_u64(effective_len, DVD_BLOCK_SIZE).min(u32::MAX as u64) as u32;
        let block_last = block_first.saturating_add(blocks.saturating_sub(1));
        out.push(AobFileEntry {
            title_set_nr,
            part_nr: part,
            file_name,
            exists,
            byte_len: len,
            block_first,
            block_last,
        });
        block_first = block_last.saturating_add(1);
    }
    Ok(out)
}

pub struct AobSectorReader<'a, V: DvdaVolume + ?Sized> {
    volume: &'a V,
    aobs: &'a [AobFileEntry],
}

impl<'a, V: DvdaVolume + ?Sized> AobSectorReader<'a, V> {
    pub fn new(volume: &'a V, aobs: &'a [AobFileEntry]) -> Self {
        Self { volume, aobs }
    }

    pub fn read_blocks(&self, block_first: u32, block_count: u32) -> Result<Vec<u8>> {
        if block_count == 0 {
            return Ok(Vec::new());
        }
        let bytes_len = (block_count as usize)
            .checked_mul(DVD_BLOCK_SIZE as usize)
            .ok_or_else(|| DvdaError::parse("AOB block read", "requested byte count overflows usize"))?;
        let mut out = vec![0u8; bytes_len];
        self.read_blocks_into(block_first, block_count, &mut out)?;
        Ok(out)
    }

    pub fn read_blocks_into(&self, block_first: u32, block_count: u32, out: &mut [u8]) -> Result<usize> {
        if block_count == 0 {
            return Ok(0);
        }
        let required = (block_count as usize)
            .checked_mul(DVD_BLOCK_SIZE as usize)
            .ok_or_else(|| DvdaError::parse("AOB block read", "requested byte count overflows usize"))?;
        if out.len() < required {
            return Err(DvdaError::ShortRead {
                context: "AOB destination buffer".to_string(),
                needed: required,
                available: out.len(),
            });
        }

        let mut remaining = block_count;
        let mut current = block_first;
        let mut written_blocks = 0u32;

        while remaining > 0 {
            let aob = self
                .aobs
                .iter()
                .find(|entry| entry.contains(current))
                .ok_or_else(|| DvdaError::parse("AOB block read", format!("logical block {current} is not backed by an AOB file")))?;
            let blocks_in_file = (aob.block_last - current + 1).min(remaining);
            let offset = ((current - aob.block_first) as u64)
                .checked_mul(DVD_BLOCK_SIZE)
                .ok_or_else(|| DvdaError::parse("AOB block read", "file offset overflow"))?;
            let read_len = (blocks_in_file as usize) * DVD_BLOCK_SIZE as usize;
            let out_off = (written_blocks as usize) * DVD_BLOCK_SIZE as usize;

            let mut file = self.volume.open_audio_ts_file(&aob.file_name)?;
            file.seek(SeekFrom::Start(offset)).map_err(|source| DvdaError::io(&aob.file_name, source))?;
            file.read_exact(&mut out[out_off..out_off + read_len])
                .map_err(|source| DvdaError::io(&aob.file_name, source))?;

            remaining -= blocks_in_file;
            current = current.saturating_add(blocks_in_file);
            written_blocks += blocks_in_file;
        }

        Ok((written_blocks as usize) * DVD_BLOCK_SIZE as usize)
    }
}

fn ceil_div_u64(n: u64, d: u64) -> u64 {
    if n == 0 { 0 } else { 1 + (n - 1) / d }
}
