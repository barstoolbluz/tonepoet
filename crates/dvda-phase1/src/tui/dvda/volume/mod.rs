#![forbid(unsafe_code)]

use std::io::{Read, Seek};

use crate::tui::dvda::error::{DvdaError, Result};

mod dir;
#[cfg(feature = "iso-isomage")]
mod iso;

pub use dir::DirectoryDvdaVolume;
#[cfg(feature = "iso-isomage")]
pub use iso::IsoDvdaVolume;

pub trait DvdaFile: Read + Seek + Send {
    fn len(&self) -> u64;
}

pub trait DvdaVolume: Send + Sync {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>>;

    fn file_len(&self, name: &str) -> Result<Option<u64>> {
        match self.open_audio_ts_file(name) {
            Ok(file) => Ok(Some(file.len())),
            Err(DvdaError::MissingFile { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn exists_audio_ts_file(&self, name: &str) -> bool {
        matches!(self.file_len(name), Ok(Some(_)))
    }

    fn read_audio_ts_file(&self, name: &str) -> Result<Vec<u8>> {
        let mut file = self.open_audio_ts_file(name)?;
        let mut out = Vec::with_capacity(file.len().min(16 * 1024 * 1024) as usize);
        file.read_to_end(&mut out).map_err(|source| DvdaError::io(name, source))?;
        Ok(out)
    }

    fn read_with_backup(&self, primary: &str, backup: &str) -> Result<(String, Vec<u8>)> {
        match self.read_audio_ts_file(primary) {
            Ok(bytes) => Ok((primary.to_string(), bytes)),
            Err(DvdaError::MissingFile { .. }) => match self.read_audio_ts_file(backup) {
                Ok(bytes) => Ok((backup.to_string(), bytes)),
                Err(DvdaError::MissingFile { .. }) => Err(DvdaError::MissingFile {
                    candidates: vec![primary.to_string(), backup.to_string()],
                }),
                Err(err) => Err(err),
            },
            Err(err) => {
                // A corrupt primary IFO should not silently fall back. That makes fixture
                // failures reproducible and keeps diagnostics tied to the damaged source.
                let _ = backup;
                Err(err)
            }
        }
    }
}
