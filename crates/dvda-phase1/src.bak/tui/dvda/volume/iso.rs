#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use isomage::{cat_node, detect_and_parse_filesystem, TreeNode};

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::volume::{DvdaFile, DvdaVolume};

#[derive(Clone, Debug)]
pub struct IsoDvdaVolume {
    iso_path: PathBuf,
    root: TreeNode,
}

impl IsoDvdaVolume {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let iso_path = path.into();
        let mut file = File::open(&iso_path)
            .map_err(|source| DvdaError::io(iso_path.display().to_string(), source))?;
        let root = detect_and_parse_filesystem(&mut file, &iso_path.to_string_lossy())
            .map_err(|err| DvdaError::Iso { message: err.to_string() })?;
        Ok(Self { iso_path, root })
    }

    pub fn iso_path(&self) -> &Path {
        &self.iso_path
    }

    fn find_audio_ts_node(&self, name: &str) -> Option<TreeNode> {
        let wanted = name.to_ascii_uppercase();
        for path in [format!("AUDIO_TS/{wanted}"), wanted.clone()] {
            if let Some(node) = self.root.find_node(&path) {
                if !node.is_directory {
                    return Some(node.clone());
                }
            }
        }
        find_case_insensitive(&self.root, &["AUDIO_TS", &wanted])
            .or_else(|| find_case_insensitive(&self.root, &[&wanted]))
    }
}

impl DvdaVolume for IsoDvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> Result<Box<dyn DvdaFile>> {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
            return Err(DvdaError::parse("AUDIO_TS filename", format!("invalid filename {name:?}")));
        }
        let node = self.find_audio_ts_node(name).ok_or_else(|| DvdaError::MissingFile {
            candidates: vec![format!("AUDIO_TS/{}", name.to_ascii_uppercase())],
        })?;
        let mut iso = File::open(&self.iso_path)
            .map_err(|source| DvdaError::io(self.iso_path.display().to_string(), source))?;
        let mut out = Vec::with_capacity(node.size.min(16 * 1024 * 1024) as usize);
        cat_node(&mut iso, &node, &mut out).map_err(|err| DvdaError::Iso { message: err.to_string() })?;
        let len = out.len() as u64;
        Ok(Box::new(MemoryDvdaFile { cursor: Cursor::new(out), len }))
    }
}

pub struct MemoryDvdaFile {
    cursor: Cursor<Vec<u8>>,
    len: u64,
}

impl DvdaFile for MemoryDvdaFile {
    fn len(&self) -> u64 {
        self.len
    }
}

impl Read for MemoryDvdaFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Seek for MemoryDvdaFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

fn find_case_insensitive(node: &TreeNode, parts: &[&str]) -> Option<TreeNode> {
    if parts.is_empty() {
        return Some(node.clone());
    }
    let (head, tail) = parts.split_first()?;
    node.children
        .iter()
        .find(|child| child.name.eq_ignore_ascii_case(head))
        .and_then(|child| find_case_insensitive(child, tail))
}
