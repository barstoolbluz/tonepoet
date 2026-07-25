//! Stable source identity and mutation detection for copy-then-delete moves.
//!
//! A pathname is not an object identity.  These helpers capture the underlying
//! filesystem object plus a conservative version token, and provide a small
//! SHA-256 implementation so callers can prove that the bytes copied are still
//! the bytes present immediately before source cleanup.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};


const MAX_MANIFEST_ENTRIES: usize = 100_000;
const MAX_MANIFEST_DEPTH: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceVersion {
    #[cfg(unix)]
    Unix {
        len: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_sec: i64,
        mtime_nsec: i64,
        ctime_sec: i64,
        ctime_nsec: i64,
    },
    #[cfg(windows)]
    Windows {
        len: u64,
        creation_time: u64,
        last_write_time: u64,
        attributes: u32,
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        len: u64,
        modified: Option<std::time::SystemTime>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    kind: SourceKind,
    identity: SourceIdentity,
    version: SourceVersion,
    symlink_target: Option<PathBuf>,
}

impl SourceSnapshot {
    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub fn len(&self) -> u64 {
        match &self.version {
            #[cfg(unix)]
            SourceVersion::Unix { len, .. } => *len,
            #[cfg(windows)]
            SourceVersion::Windows { len, .. } => *len,
            #[cfg(not(any(unix, windows)))]
            SourceVersion::Portable { len, .. } => *len,
        }
    }

    pub fn supports_identity_proof(&self) -> bool {
        #[cfg(any(unix, windows))]
        {
            true
        }
        #[cfg(not(any(unix, windows)))]
        {
            !matches!(&self.identity, SourceIdentity::Unsupported)
        }
    }

    pub fn verify_same_identity(&self, current: &Self) -> Result<(), String> {
        if self.kind != current.kind {
            return Err(format!("source kind changed from {:?} to {:?}", self.kind, current.kind));
        }
        if self.identity != current.identity {
            return Err("source object identity changed".to_string());
        }
        Ok(())
    }

    pub fn verify_same_object_after_rename(&self, current: &Self) -> Result<(), String> {
        self.verify_same_identity(current)?;
        let stable = match (&self.version, &current.version) {
            #[cfg(unix)]
            (
                SourceVersion::Unix {
                    len: left_len,
                    mode: left_mode,
                    uid: left_uid,
                    gid: left_gid,
                    mtime_sec: left_mtime_sec,
                    mtime_nsec: left_mtime_nsec,
                    ..
                },
                SourceVersion::Unix {
                    len: right_len,
                    mode: right_mode,
                    uid: right_uid,
                    gid: right_gid,
                    mtime_sec: right_mtime_sec,
                    mtime_nsec: right_mtime_nsec,
                    ..
                },
            ) => {
                left_len == right_len
                    && left_mode == right_mode
                    && left_uid == right_uid
                    && left_gid == right_gid
                    && left_mtime_sec == right_mtime_sec
                    && left_mtime_nsec == right_mtime_nsec
            }
            #[cfg(windows)]
            (SourceVersion::Windows { .. }, SourceVersion::Windows { .. }) => {
                self.version == current.version
            }
            #[cfg(not(any(unix, windows)))]
            (SourceVersion::Portable { .. }, SourceVersion::Portable { .. }) => {
                self.version == current.version
            }
        };
        if stable {
            Ok(())
        } else {
            Err("source metadata/content change token changed".to_string())
        }
    }

    pub fn verify_same_object_and_version(&self, current: &Self) -> Result<(), String> {
        if self.kind != current.kind {
            return Err(format!(
                "source kind changed from {:?} to {:?}",
                self.kind, current.kind
            ));
        }
        if self.identity != current.identity {
            return Err("source object identity changed".to_string());
        }
        if self.version != current.version {
            return Err("source size or filesystem change token changed".to_string());
        }
        if self.symlink_target != current.symlink_target {
            return Err("source symlink target changed".to_string());
        }
        Ok(())
    }
}

pub fn snapshot_path(path: &Path) -> io::Result<SourceSnapshot> {
    let metadata = fs::symlink_metadata(path)?;
    let kind = kind_from_metadata(&metadata)?;
    let symlink_target = if kind == SourceKind::Symlink {
        Some(fs::read_link(path)?)
    } else {
        None
    };
    let identity = path_identity(path, kind, &metadata)?;
    Ok(SourceSnapshot {
        kind,
        identity,
        version: version_from_metadata(&metadata),
        symlink_target,
    })
}

pub fn snapshot_open_file(file: &File) -> io::Result<SourceSnapshot> {
    let metadata = file.metadata()?;
    let kind = kind_from_metadata(&metadata)?;
    if kind != SourceKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened handle is not a regular file",
        ));
    }
    Ok(SourceSnapshot {
        kind,
        identity: file_identity(file, &metadata)?,
        version: version_from_metadata(&metadata),
        symlink_target: None,
    })
}

pub fn verify_path(path: &Path, expected: &SourceSnapshot) -> Result<(), String> {
    let current = snapshot_path(path)
        .map_err(|error| format!("could not re-read source identity: {error}"))?;
    expected.verify_same_object_and_version(&current)
}

/// Preserve regular-file metadata from one open handle to another.
///
/// Source metadata is read from the already-open source object, never by
/// re-resolving its pathname. Destination metadata is applied to the already-
/// open staged/reserved object. Failures are returned as explicit fidelity
/// warnings because content publication remains independently verifiable.
pub fn preserve_open_file_metadata(source: &File, destination: &File) -> Vec<String> {
    let mut warnings = Vec::new();
    let metadata = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            warnings.push(format!("read source metadata from open handle: {error}"));
            return warnings;
        }
    };

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let source_fd = source.as_raw_fd();
        let destination_fd = destination.as_raw_fd();
        if unsafe { libc::fchown(destination_fd, metadata.uid(), metadata.gid()) } != 0 {
            warnings.push(format!("ownership: {}", io::Error::last_os_error()));
        }
        let times = [
            libc::timespec {
                tv_sec: metadata.atime(),
                tv_nsec: metadata.atime_nsec() as _,
            },
            libc::timespec {
                tv_sec: metadata.mtime(),
                tv_nsec: metadata.mtime_nsec() as _,
            },
        ];
        if unsafe { libc::futimens(destination_fd, times.as_ptr()) } != 0 {
            warnings.push(format!("timestamps: {}", io::Error::last_os_error()));
        }

        #[cfg(target_os = "linux")]
        warnings.extend(copy_linux_xattrs_between_fds(source_fd, destination_fd));

        // Ownership and ACL writes can clear set-ID bits, so permissions are
        // restored last.
        if unsafe { libc::fchmod(destination_fd, metadata.mode() & 0o7777) } != 0 {
            warnings.push(format!("permissions: {}", io::Error::last_os_error()));
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = destination.set_permissions(metadata.permissions()) {
        warnings.push(format!("permissions: {error}"));
    }

    warnings
}

#[cfg(target_os = "linux")]
fn copy_linux_xattrs_between_fds(source_fd: i32, destination_fd: i32) -> Vec<String> {
    use std::ffi::CString;

    let mut warnings = Vec::new();
    let size = unsafe { libc::flistxattr(source_fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EOPNOTSUPP) {
            warnings.push(format!("list extended attributes: {error}"));
        }
        return warnings;
    }
    if size == 0 {
        return warnings;
    }

    let mut names = vec![0u8; size as usize];
    let read = unsafe { libc::flistxattr(source_fd, names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        warnings.push(format!(
            "read extended attribute names: {}",
            io::Error::last_os_error()
        ));
        return warnings;
    }

    for raw_name in names[..read as usize]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = match CString::new(raw_name) {
            Ok(name) => name,
            Err(_) => {
                warnings.push("extended attribute name contains NUL".to_string());
                continue;
            }
        };
        let value_size = unsafe {
            libc::fgetxattr(source_fd, name.as_ptr(), std::ptr::null_mut(), 0)
        };
        if value_size < 0 {
            warnings.push(format!(
                "read extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
            continue;
        }
        let mut value = vec![0u8; value_size as usize];
        if value_size > 0 {
            let got = unsafe {
                libc::fgetxattr(
                    source_fd,
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            if got < 0 {
                warnings.push(format!(
                    "read extended attribute {:?}: {}",
                    String::from_utf8_lossy(raw_name),
                    io::Error::last_os_error()
                ));
                continue;
            }
            value.truncate(got as usize);
        }
        if unsafe {
            libc::fsetxattr(
                destination_fd,
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } != 0
        {
            warnings.push(format!(
                "write extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
        }
    }
    warnings
}

fn kind_from_metadata(metadata: &fs::Metadata) -> io::Result<SourceKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok(SourceKind::File)
    } else if file_type.is_dir() {
        Ok(SourceKind::Directory)
    } else if file_type.is_symlink() {
        Ok(SourceKind::Symlink)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "special filesystem objects are not supported",
        ))
    }
}

#[cfg(unix)]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    use std::os::unix::fs::MetadataExt;
    SourceVersion::Unix {
        len: metadata.len(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    use std::os::windows::fs::MetadataExt;
    SourceVersion::Windows {
        len: metadata.len(),
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
        attributes: metadata.file_attributes(),
    }
}

#[cfg(not(any(unix, windows)))]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    SourceVersion::Portable {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

#[cfg(unix)]
fn path_identity(
    _path: &Path,
    _kind: SourceKind,
    metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(SourceIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(SourceIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn path_identity(
    path: &Path,
    kind: SourceKind,
    _metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    let mut flags = 0;
    if kind == SourceKind::Directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    if kind == SourceKind::Symlink {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
    }
    let file = options.custom_flags(flags).open(path)?;
    windows_file_identity(&file)
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    windows_file_identity(file)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<SourceIdentity> {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    let ok = unsafe {
        GetFileInformationByHandle(
            file.as_raw_handle().cast(),
            information.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(SourceIdentity::Windows {
        volume_serial: information.volume_serial_number,
        file_index: ((information.file_index_high as u64) << 32)
            | information.file_index_low as u64,
    })
}

#[cfg(not(any(unix, windows)))]
fn path_identity(
    _path: &Path,
    _kind: SourceKind,
    _metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    Ok(SourceIdentity::Unsupported)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    Ok(SourceIdentity::Unsupported)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentDigest(pub [u8; 32]);

impl ContentDigest {
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667,
                0xbb67ae85,
                0x3c6ef372,
                0xa54ff53a,
                0x510e527f,
                0x9b05688c,
                0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.block_len != 0 {
            let needed = 64 - self.block_len;
            let take = needed.min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == 64 {
                let block = self.block;
                self.compress(&block);
                self.block_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.block[..data.len()].copy_from_slice(data);
            self.block_len = data.len();
        }
    }

    pub fn finalize(mut self) -> ContentDigest {
        let bit_len = self.total_len.wrapping_mul(8);
        self.block[self.block_len] = 0x80;
        self.block_len += 1;
        if self.block_len > 56 {
            for byte in &mut self.block[self.block_len..] {
                *byte = 0;
            }
            let block = self.block;
            self.compress(&block);
            self.block = [0; 64];
            self.block_len = 0;
        }
        for byte in &mut self.block[self.block_len..56] {
            *byte = 0;
        }
        self.block[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.block;
        self.compress(&block);

        let mut output = [0u8; 32];
        for (chunk, word) in output.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        ContentDigest(output)
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
            0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
            0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
            0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
            0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
            0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
            0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
            0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for (index, chunk) in block.chunks_exact(4).take(16).enumerate() {
            w[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        let mut digest = Sha256::new();
        digest.update(b"abc");
        assert_eq!(
            digest.finalize().to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty_and_split_updates_match_known_vectors() {
        assert_eq!(
            Sha256::new().finalize().to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let mut split = Sha256::new();
        split.update(b"a");
        split.update(b"b");
        split.update(b"c");
        assert_eq!(
            split.finalize().to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pathname_replacement_is_not_the_captured_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write original");
        let captured = snapshot_path(&source).expect("capture source");

        fs::rename(&source, &displaced).expect("displace original");
        fs::write(&source, b"replaced").expect("write same-size replacement");

        let error = verify_path(&source, &captured).expect_err("replacement must be rejected");
        assert!(error.contains("identity changed"), "unexpected error: {error}");
    }

    #[test]
    fn manifest_rejects_same_object_content_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        fs::write(&source, b"original").expect("write original");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::write(&source, b"mutated!").expect("mutate source");

        let error = manifest.verify_at(&source).expect_err("mutation must be rejected");
        assert!(
            error.contains("changed") || error.contains("digest"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn manifest_rejects_same_size_replacement_after_quarantine() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let original = temp.path().join("original.bin");
        let quarantine = temp.path().join("quarantine.bin");
        fs::write(&source, b"original").expect("write original");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::rename(&source, &original).expect("displace original");
        fs::write(&source, b"replaced").expect("write same-size replacement");
        fs::rename(&source, &quarantine).expect("quarantine replacement");

        let error = manifest
            .verify_at(&quarantine)
            .expect_err("replacement must not authorize cleanup");
        assert!(
            error.contains("identity") || error.contains("digest"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&quarantine).expect("replacement retained"), b"replaced");
        assert_eq!(fs::read(&original).expect("original retained"), b"original");
    }

    #[test]
    fn directory_manifest_rejects_unplanned_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        fs::create_dir(&source).expect("create source");
        fs::write(source.join("track.flac"), b"audio").expect("write track");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::write(source.join("late.flac"), b"late").expect("write late entry");

        let error = manifest
            .verify_at(&source)
            .expect_err("new entry must prevent cleanup");
        assert!(error.contains("membership"), "unexpected error: {error}");
    }

    #[test]
    fn final_destination_entry_proof_rejects_same_size_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write source");
        let manifest = capture_manifest(&source).expect("capture manifest");
        fs::copy(&source, &destination).expect("copy destination");
        manifest
            .verify_copy_entry_at(Path::new(""), &destination)
            .expect("initial destination proof");

        fs::rename(&destination, &displaced).expect("displace copied destination");
        fs::write(&destination, b"replaced").expect("write same-size replacement");

        let error = manifest
            .verify_copy_entry_at(Path::new(""), &destination)
            .expect_err("replacement must revoke source-deletion authority");
        assert!(error.contains("digest"), "unexpected error: {error}");
        assert_eq!(fs::read(&destination).expect("replacement retained"), b"replaced");
        assert_eq!(fs::read(&displaced).expect("original copy retained"), b"original");
    }

    #[test]
    fn destination_identity_manifest_rejects_same_content_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write source");
        let source_manifest = capture_manifest(&source).expect("capture source manifest");
        fs::copy(&source, &destination).expect("copy destination");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("capture verified destination identity");

        fs::rename(&destination, &displaced).expect("displace verified destination");
        fs::write(&destination, b"original").expect("write same-content replacement");

        let error = destination_manifest
            .verify_entry_at(&source_manifest, Path::new(""), &destination)
            .expect_err("same-content replacement must revoke destination ownership");
        assert!(error.contains("identity"), "unexpected error: {error}");
        assert_eq!(fs::read(&destination).expect("replacement retained"), b"original");
        assert_eq!(fs::read(&displaced).expect("verified copy retained"), b"original");
    }
}

#[derive(Debug, Clone)]
pub struct SourceEntryProof {
    pub snapshot: SourceSnapshot,
    pub digest: Option<ContentDigest>,
}

#[derive(Debug, Clone, Default)]
pub struct SourceManifest {
    entries: std::collections::BTreeMap<PathBuf, SourceEntryProof>,
}

/// Identity/version snapshots for the exact destination objects that passed
/// whole-tree content verification. A later source cleanup step must satisfy
/// both this destination-ownership proof and the source manifest's content
/// proof before it may remove the corresponding quarantined source object.
#[derive(Debug, Clone, Default)]
pub struct DestinationManifest {
    entries: std::collections::BTreeMap<PathBuf, SourceSnapshot>,
}

impl DestinationManifest {
    pub fn verify_entry_at(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_entry_at_with_cancel(
            source_manifest,
            relative_path,
            path,
            &mut keep_going,
        )
    }

    pub fn verify_entry_at_with_cancel<F>(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        let expected_destination = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no captured identity proof: {}",
                relative_path.display()
            )
        })?;
        let source_proof = source_manifest.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no source content proof: {}",
                relative_path.display()
            )
        })?;
        let current_destination = verify_destination_entry(path, source_proof, keep_going)?;
        expected_destination
            .verify_same_object_and_version(&current_destination)
            .map_err(|error| {
                format!(
                    "destination object changed after initial verification at {}: {error}",
                    path.display()
                )
            })
    }
}

impl SourceManifest {
    pub fn root_kind(&self) -> Option<SourceKind> {
        self.entries.get(Path::new("")).map(|entry| entry.snapshot.kind())
    }

    pub fn expected_snapshot(&self, relative_path: &Path) -> Option<&SourceSnapshot> {
        self.entries.get(relative_path).map(|entry| &entry.snapshot)
    }

    /// Revalidate one manifest entry at its current pathname.
    ///
    /// This is intended for the final cleanup gate after a source root has
    /// been atomically quarantined. Regular files are opened and hashed from
    /// the handle, the handle is rechecked for mutation, and the pathname is
    /// proven to still name that handle before the caller may unlink it.
    pub fn verify_entry_at(
        &self,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_entry_at_with_cancel(relative_path, path, &mut keep_going)
    }

    pub fn verify_entry_at_with_cancel<F>(
        &self,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        let proof = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "unplanned source entry appeared during cleanup: {}",
                relative_path.display()
            )
        })?;
        verify_source_entry(path, proof, keep_going)
    }

    /// Revalidate one destination entry against the source proof used to copy it.
    /// This supports a final destination-presence/content gate immediately before
    /// the corresponding source object is removed.
    pub fn verify_copy_entry_at(
        &self,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_copy_entry_at_with_cancel(relative_path, path, &mut keep_going)
    }

    pub fn verify_copy_entry_at_with_cancel<F>(
        &self,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        let proof = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no source proof: {}",
                relative_path.display()
            )
        })?;
        verify_destination_entry(path, proof, keep_going).map(|_| ())
    }

    pub fn insert(
        &mut self,
        relative_path: PathBuf,
        snapshot: SourceSnapshot,
        digest: Option<ContentDigest>,
    ) -> Result<(), String> {
        if snapshot.kind() == SourceKind::File && digest.is_none() {
            return Err(format!(
                "regular-file proof is missing a content digest: {}",
                relative_path.display()
            ));
        }
        if !snapshot.supports_identity_proof() {
            return Err(format!(
                "stable source identity is unavailable on this platform for {}",
                relative_path.display()
            ));
        }
        if self
            .entries
            .insert(relative_path.clone(), SourceEntryProof { snapshot, digest })
            .is_some()
        {
            return Err(format!(
                "duplicate source manifest entry: {}",
                relative_path.display()
            ));
        }
        Ok(())
    }

    pub fn verify_copy_at(&self, root: &Path) -> Result<(), String> {
        self.capture_verified_copy_at(root).map(|_| ())
    }

    pub fn verify_copy_at_with_cancel<F>(
        &self,
        root: &Path,
        keep_going: F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.capture_verified_copy_at_with_cancel(root, keep_going)
            .map(|_| ())
    }

    pub fn capture_verified_copy_at(
        &self,
        root: &Path,
    ) -> Result<DestinationManifest, String> {
        self.capture_verified_copy_at_with_cancel(root, |_: &Path| true)
    }

    pub fn capture_verified_copy_at_with_cancel<F>(
        &self,
        root: &Path,
        mut keep_going: F,
    ) -> Result<DestinationManifest, String>
    where
        F: FnMut(&Path) -> bool,
    {
        let actual = enumerate_relative_paths_with_cancel(
            root,
            self.entries.len().saturating_add(1),
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            self.entries.keys().cloned().collect();
        if actual != expected {
            return Err(
                "destination tree membership does not match the copied source manifest"
                    .to_string(),
            );
        }

        let mut destination_manifest = DestinationManifest::default();
        for (relative, proof) in &self.entries {
            let path = if relative.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(relative)
            };
            if !keep_going(&path) {
                return Err("destination verification was interrupted".to_string());
            }
            let snapshot = verify_destination_entry(&path, proof, &mut keep_going)?;
            if destination_manifest
                .entries
                .insert(relative.clone(), snapshot)
                .is_some()
            {
                return Err(format!(
                    "duplicate destination manifest entry: {}",
                    relative.display()
                ));
            }
        }
        Ok(destination_manifest)
    }

    pub fn verify_at(&self, root: &Path) -> Result<(), String> {
        self.verify_at_with_cancel(root, |_: &Path| true)
    }

    pub fn verify_at_with_cancel<F>(
        &self,
        root: &Path,
        mut keep_going: F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        if !self.entries.contains_key(Path::new("")) {
            return Err("source manifest has no root entry".to_string());
        }
        let actual = enumerate_relative_paths_with_cancel(
            root,
            self.entries.len().saturating_add(1),
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            self.entries.keys().cloned().collect();
        if actual != expected {
            let added: Vec<String> = actual
                .difference(&expected)
                .take(8)
                .map(|path| path.display().to_string())
                .collect();
            let missing: Vec<String> = expected
                .difference(&actual)
                .take(8)
                .map(|path| path.display().to_string())
                .collect();
            return Err(format!(
                "source tree membership changed (unexpected: [{}]; missing: [{}])",
                added.join(", "),
                missing.join(", ")
            ));
        }

        for (relative, proof) in &self.entries {
            let path = if relative.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(relative)
            };
            verify_source_entry(&path, proof, &mut keep_going)?;
        }
        Ok(())
    }
}

fn verify_destination_entry<F>(
    path: &Path,
    proof: &SourceEntryProof,
    keep_going: &mut F,
) -> Result<SourceSnapshot, String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err("destination verification was interrupted".to_string());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("stat destination {}: {error}", path.display()))?;
    let kind = kind_from_metadata(&metadata)
        .map_err(|error| format!("classify destination {}: {error}", path.display()))?;
    if kind != proof.snapshot.kind() {
        return Err(format!("destination kind mismatch at {}", path.display()));
    }
    match kind {
        SourceKind::File => {
            let mut file = File::open(path)
                .map_err(|error| format!("open destination {}: {error}", path.display()))?;
            let opened = snapshot_open_file(&file)
                .map_err(|error| format!("identify destination {}: {error}", path.display()))?;
            let digest = digest_open_file_with_cancel(&mut file, path, keep_going)
                .map_err(|error| format!("digest destination {}: {error}", path.display()))?;
            let after = snapshot_open_file(&file)
                .map_err(|error| format!("re-identify destination {}: {error}", path.display()))?;
            opened.verify_same_object_and_version(&after).map_err(|error| {
                format!(
                    "destination changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            let path_snapshot = snapshot_path(path)
                .map_err(|error| format!("re-identify destination path {}: {error}", path.display()))?;
            opened.verify_same_identity(&path_snapshot).map_err(|error| {
                format!(
                    "destination path changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            if proof.digest != Some(digest) {
                return Err(format!(
                    "destination content digest mismatch at {}",
                    path.display()
                ));
            }
            return Ok(path_snapshot);
        }
        SourceKind::Symlink => {
            let before = snapshot_path(path)
                .map_err(|error| format!("identify destination symlink {}: {error}", path.display()))?;
            let target = fs::read_link(path)
                .map_err(|error| format!("read destination symlink {}: {error}", path.display()))?;
            let after = snapshot_path(path)
                .map_err(|error| format!("re-identify destination symlink {}: {error}", path.display()))?;
            before.verify_same_object_and_version(&after).map_err(|error| {
                format!(
                    "destination symlink changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            if proof.snapshot.symlink_target.as_ref() != Some(&target) {
                return Err(format!(
                    "destination symlink target mismatch at {}",
                    path.display()
                ));
            }
            return Ok(after);
        }
        SourceKind::Directory => {}
    }
    snapshot_path(path)
        .map_err(|error| format!("identify destination directory {}: {error}", path.display()))
}

fn verify_source_entry<F>(
    path: &Path,
    proof: &SourceEntryProof,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err("source verification was interrupted".to_string());
    }
    match proof.snapshot.kind() {
        SourceKind::File => {
            let mut file = File::open(path)
                .map_err(|error| format!("open {} for verification: {error}", path.display()))?;
            let before = snapshot_open_file(&file)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            proof
                .snapshot
                .verify_same_object_after_rename(&before)
                .map_err(|error| format!("{} changed before cleanup: {error}", path.display()))?;
            let digest = digest_open_file_with_cancel(&mut file, path, keep_going)
                .map_err(|error| format!("digest {}: {error}", path.display()))?;
            let after = snapshot_open_file(&file)
                .map_err(|error| format!("re-identify {}: {error}", path.display()))?;
            before
                .verify_same_object_and_version(&after)
                .map_err(|error| {
                    format!("{} changed while being verified: {error}", path.display())
                })?;
            let pathname = snapshot_path(path)
                .map_err(|error| format!("re-identify path {}: {error}", path.display()))?;
            after.verify_same_identity(&pathname).map_err(|error| {
                format!(
                    "{} pathname changed while its opened object was being verified: {error}",
                    path.display()
                )
            })?;
            if proof.digest != Some(digest) {
                return Err(format!(
                    "{} content digest changed before cleanup",
                    path.display()
                ));
            }
        }
        SourceKind::Directory => {
            let current = snapshot_path(path)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            proof
                .snapshot
                .verify_same_object_after_rename(&current)
                .map_err(|error| format!("{} changed before cleanup: {error}", path.display()))?;
        }
        SourceKind::Symlink => {
            let before = snapshot_path(path)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            proof
                .snapshot
                .verify_same_object_after_rename(&before)
                .map_err(|error| format!("{} changed before cleanup: {error}", path.display()))?;
            let target = fs::read_link(path)
                .map_err(|error| format!("read symlink {}: {error}", path.display()))?;
            let after = snapshot_path(path)
                .map_err(|error| format!("re-identify symlink {}: {error}", path.display()))?;
            before
                .verify_same_object_and_version(&after)
                .map_err(|error| {
                    format!("{} changed while being verified: {error}", path.display())
                })?;
            if proof.snapshot.symlink_target.as_ref() != Some(&target) {
                return Err(format!(
                    "{} symlink target changed before cleanup",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

pub fn digest_open_file(file: &mut File) -> io::Result<ContentDigest> {
    let mut keep_going = |_: &Path| true;
    digest_open_file_with_cancel(file, Path::new(""), &mut keep_going)
}

fn digest_open_file_with_cancel<F>(
    file: &mut File,
    path: &Path,
    keep_going: &mut F,
) -> io::Result<ContentDigest>
where
    F: FnMut(&Path) -> bool,
{
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut sha = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if !keep_going(path) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "content verification was interrupted",
            ));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha.update(&buffer[..read]);
    }
    Ok(sha.finalize())
}

fn enumerate_relative_paths_with_cancel<F>(
    root: &Path,
    maximum_entries: usize,
    keep_going: &mut F,
) -> Result<std::collections::BTreeSet<PathBuf>, String>
where
    F: FnMut(&Path) -> bool,
{
    let limit = maximum_entries.min(MAX_MANIFEST_ENTRIES.saturating_add(1));
    let mut result = std::collections::BTreeSet::new();
    result.insert(PathBuf::new());
    enumerate_relative_paths_inner(root, Path::new(""), 0, limit, &mut result, keep_going)?;
    Ok(result)
}

fn enumerate_relative_paths_inner<F>(
    absolute: &Path,
    relative: &Path,
    depth: usize,
    maximum_entries: usize,
    result: &mut std::collections::BTreeSet<PathBuf>,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(absolute) {
        return Err("filesystem tree verification was interrupted".to_string());
    }
    if depth > MAX_MANIFEST_DEPTH {
        return Err(format!(
            "filesystem tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
            absolute.display()
        ));
    }
    let metadata = fs::symlink_metadata(absolute)
        .map_err(|error| format!("stat {}: {error}", absolute.display()))?;
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(absolute)
        .map_err(|error| format!("read directory {}: {error}", absolute.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("read directory entry {}: {error}", absolute.display()))?;
        if !keep_going(&entry.path()) {
            return Err("filesystem tree verification was interrupted".to_string());
        }
        let child_relative = relative.join(entry.file_name());
        if !result.insert(child_relative.clone()) {
            return Err(format!(
                "duplicate directory entry while verifying {}",
                child_relative.display()
            ));
        }
        if result.len() > maximum_entries {
            return Err(format!(
                "filesystem tree contains more entries than the bounded verification limit of {maximum_entries}"
            ));
        }
        enumerate_relative_paths_inner(
            &entry.path(),
            &child_relative,
            depth + 1,
            maximum_entries,
            result,
            keep_going,
        )?;
    }
    Ok(())
}

pub fn capture_manifest(root: &Path) -> Result<SourceManifest, String> {
    capture_manifest_with_cancel(root, |_: &Path| true)
}

pub fn capture_manifest_with_cancel<F>(
    root: &Path,
    mut keep_going: F,
) -> Result<SourceManifest, String>
where
    F: FnMut(&Path) -> bool,
{
    fn capture_node<F>(
        root: &Path,
        path: &Path,
        manifest: &mut SourceManifest,
        entries: &mut usize,
        depth: usize,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        if !keep_going(path) {
            return Err("source manifest capture was interrupted".to_string());
        }
        if depth > MAX_MANIFEST_DEPTH {
            return Err(format!(
                "source tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
                path.display()
            ));
        }
        *entries = entries.saturating_add(1);
        if *entries > MAX_MANIFEST_ENTRIES {
            return Err(format!(
                "source tree exceeds the bounded manifest limit of {MAX_MANIFEST_ENTRIES} entries; split the move into smaller roots"
            ));
        }
        let before = snapshot_path(path)
            .map_err(|error| format!("capture source {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("source escaped manifest root: {}", path.display()))?
            .to_path_buf();
        match before.kind() {
            SourceKind::File => {
                use std::io::Read;
                let mut file = File::open(path)
                    .map_err(|error| format!("open source {}: {error}", path.display()))?;
                let opened = snapshot_open_file(&file)
                    .map_err(|error| format!("identify opened source {}: {error}", path.display()))?;
                before.verify_same_object_and_version(&opened).map_err(|error| {
                    format!("source changed before manifest capture {}: {error}", path.display())
                })?;
                let mut sha = Sha256::new();
                let mut buffer = vec![0u8; 1024 * 1024];
                loop {
                    if !keep_going(path) {
                        return Err("source manifest capture was interrupted".to_string());
                    }
                    let read = file
                        .read(&mut buffer)
                        .map_err(|error| format!("read source {}: {error}", path.display()))?;
                    if read == 0 {
                        break;
                    }
                    sha.update(&buffer[..read]);
                }
                let digest = sha.finalize();
                let after = snapshot_open_file(&file)
                    .map_err(|error| format!("re-identify source {}: {error}", path.display()))?;
                opened.verify_same_object_and_version(&after).map_err(|error| {
                    format!("source changed during manifest capture {}: {error}", path.display())
                })?;
                manifest.insert(relative, before, Some(digest))?;
            }
            SourceKind::Symlink => {
                manifest.insert(relative, before, None)?;
            }
            SourceKind::Directory => {
                manifest.insert(relative, before.clone(), None)?;
                let directory_entries = fs::read_dir(path)
                    .map_err(|error| format!("read source directory {}: {error}", path.display()))?;
                for entry in directory_entries {
                    let entry = entry
                        .map_err(|error| format!("read source entry {}: {error}", path.display()))?;
                    capture_node(
                        root,
                        &entry.path(),
                        manifest,
                        entries,
                        depth + 1,
                        keep_going,
                    )?;
                }
                let after = snapshot_path(path)
                    .map_err(|error| format!("re-identify directory {}: {error}", path.display()))?;
                before.verify_same_object_and_version(&after).map_err(|error| {
                    format!("source directory changed during manifest capture {}: {error}", path.display())
                })?;
            }
        }
        Ok(())
    }

    let mut manifest = SourceManifest::default();
    let mut entries = 0usize;
    capture_node(root, root, &mut manifest, &mut entries, 0, &mut keep_going)?;
    Ok(manifest)
}
