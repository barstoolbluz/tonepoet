#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::volume::{DvdaFile, DvdaVolume};

#[derive(Clone, Debug)]
pub struct DirectoryDvdaVolume {
    root: PathBuf,
}

impl DirectoryDvdaVolume {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve_audio_ts_file(&self, name: &str) -> Result<PathBuf> {
        let name = sanitize_audio_ts_name(name)?;
        let mut candidates = Vec::new();
        candidates.push(self.root.join("AUDIO_TS").join(&name));
        candidates.push(self.root.join(&name));

        for candidate in &candidates {
            if candidate.is_file() {
                return Ok(candidate.clone());
            }
        }

        if let Some(path) = find_case_insensitive(&self.root.join("AUDIO_TS"), &name)? {
            return Ok(path);
        }
        if let Some(path) = find_case_insensitive(&self.root, &name)? {
            return Ok(path);
        }

        Err(DvdaError::MissingFile {
            candidates: candidates.into_iter().map(|p| p.display().to_string()).collect(),
        })
    }
}

impl DvdaVolume for DirectoryDvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
        let path = self.resolve_audio_ts_file(name)?;
        let file = File::open(&path).map_err(|source| DvdaError::io(path.display().to_string(), source))?;
        let len = file.metadata().map_err(|source| DvdaError::io(path.display().to_string(), source))?.len();
        Ok(Box::new(LocalDvdaFile { file, len }))
    }
}

pub struct LocalDvdaFile {
    file: File,
    len: u64,
}

impl DvdaFile for LocalDvdaFile {
    fn len(&self) -> u64 {
        self.len
    }
}

impl Read for LocalDvdaFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for LocalDvdaFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

fn sanitize_audio_ts_name(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(DvdaError::parse("AUDIO_TS filename", format!("invalid filename {name:?}")));
    }
    Ok(name.to_ascii_uppercase())
}

fn find_case_insensitive(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(DvdaError::io(dir.display().to_string(), source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| DvdaError::io(dir.display().to_string(), source))?;
        let file_name = entry.file_name();
        if file_name.to_string_lossy().eq_ignore_ascii_case(name) && entry.path().is_file() {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}
