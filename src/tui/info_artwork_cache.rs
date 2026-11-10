//! Disposable, Info-pane-only image previews. No viewer or metadata-editor path
//! calls this module. Decoded pixels are owned by a small in-process LRU; WebP
//! derivatives live in a separate SQLite database under the user's cache dir.
//!
//! All source metadata and SQLite I/O happen on the existing decode worker.
//! WebP encoding, mutations, eviction and VACUUM run on one bounded writer.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::{codecs::webp::WebPEncoder, DynamicImage, ImageEncoder, ImageFormat, ImageReader, Limits};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::browse_artwork::ArtworkSource;

const MEMORY_BUDGET: usize = 32 * 1024 * 1024;
const DISK_BUDGET: i64 = 256 * 1024 * 1024;
const ENTRY_BUDGET: usize = 2 * 1024 * 1024;
// 768 px suits conventional Browse Info cell geometry and keeps most WebP
// derivatives small. High-entropy RGBA can exceed the 2 MiB disk-entry cap
// and is then retained in memory only.
pub(super) const PREVIEW_EDGE: u32 = 768;
const FORMAT_VERSION: i64 = 1;
const MAX_QUEUED_WRITES: usize = 8;
// A 32K-track folder is legal in Browse but should not create a giant
// predecessor-stamp index for one cached embedded picture.
const MAX_EMBEDDED_STAMPS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceStamp {
    path: PathBuf,
    len: u64,
    modified_seconds: i64,
    modified_nanos: u32,
    device: u64,
    inode: u64,
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() }
    else { std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path)) }
}

fn stamp(path: &Path) -> Option<SourceStamp> {
    let path = absolute_path(path);
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() { return None; }
    let (modified_seconds, modified_nanos) = match metadata.modified().ok()?.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => (i64::try_from(elapsed.as_secs()).ok()?, elapsed.subsec_nanos()),
        Err(before) => (-i64::try_from(before.duration().as_secs()).ok()?, before.duration().subsec_nanos()),
    };
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    Some(SourceStamp { path, len: metadata.len(), modified_seconds, modified_nanos, device, inode })
}

/// For an embedded-artwork fallback, record *every* preceding candidate. A
/// higher-priority track acquiring a picture invalidates the cached fallback
/// even if the old winning track is unchanged. No negative artwork result is
/// cached independently.
pub(super) fn stamps_for_decoded(source: &ArtworkSource, embedded_path: Option<&Path>) -> Option<Vec<SourceStamp>> {
    match source {
        ArtworkSource::File(path) => Some(vec![stamp(path)?]),
        ArtworkSource::Embedded(paths) => {
            let selected = embedded_path?;
            let last = paths.iter().position(|path| path == selected)?;
            if last >= MAX_EMBEDDED_STAMPS { return None; }
            paths[..=last].iter().map(|path| stamp(path)).collect()
        }
    }
}

fn matches_source(source: &ArtworkSource, stamps: &[SourceStamp]) -> bool {
    if stamps.is_empty() { return false; }
    let matches = |path: &Path, expected: &SourceStamp| {
        absolute_path(path) == expected.path && stamp(path).as_ref() == Some(expected)
    };
    match source {
        ArtworkSource::File(path) => stamps.len() == 1 && matches(path, &stamps[0]),
        ArtworkSource::Embedded(paths) => stamps.len() <= MAX_EMBEDDED_STAMPS
            && stamps.len() <= paths.len()
            && paths.iter().zip(stamps).all(|(path, expected)| matches(path, expected)),
    }
}

pub(super) fn source_key(source: &ArtworkSource) -> String {
    let mut hash = Sha256::new();
    let feed_path = |hash: &mut Sha256, path: &Path| {
        let absolute = absolute_path(path);
        let bytes = absolute.as_os_str().as_encoded_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    match source {
        ArtworkSource::File(path) => {
            hash.update(b"info-file-v1");
            feed_path(&mut hash, path);
        }
        ArtworkSource::Embedded(paths) => {
            hash.update(b"info-embedded-v1");
            for path in paths.iter() { feed_path(&mut hash, path); }
        }
    }
    hex::encode(hash.finalize())
}

#[derive(Clone)]
pub(super) struct Preview {
    pub pixels: Arc<DynamicImage>,
    pub embedded_path: Option<PathBuf>,
    pub stamps: Vec<SourceStamp>,
}

impl Preview {
    fn memory_bytes(&self) -> usize {
        // DynamicImage may contain 16-bit or float channels, not just RGBA8.
        // Count the actual backing allocation rather than assuming 4 B/pixel.
        let pixels = self.pixels.as_bytes().len();
        let identity = self.stamps.iter().map(|s| std::mem::size_of::<SourceStamp>()
            .saturating_add(s.path.as_os_str().as_encoded_bytes().len())).sum::<usize>();
        pixels.saturating_add(identity)
    }
}

struct MemoryEntry { key: String, preview: Preview, bytes: usize }

#[derive(Default)]
struct MemoryLru { entries: VecDeque<MemoryEntry>, used: usize }

impl MemoryLru {
    fn remove(&mut self, key: &str) {
        if let Some(i) = self.entries.iter().position(|v| v.key == key) {
            if let Some(entry) = self.entries.remove(i) { self.used -= entry.bytes; }
        }
    }
    fn get(&mut self, key: &str) -> Option<Preview> {
        let i = self.entries.iter().position(|v| v.key == key)?;
        let entry = self.entries.remove(i)?;
        let preview = entry.preview.clone(); // Arc, not pixel copy
        self.entries.push_back(entry);
        Some(preview)
    }
    fn put(&mut self, key: String, preview: Preview) {
        self.remove(&key);
        let bytes = preview.memory_bytes().saturating_add(key.len());
        if bytes > MEMORY_BUDGET { return; }
        while self.used.saturating_add(bytes) > MEMORY_BUDGET {
            let Some(entry) = self.entries.pop_front() else { break; };
            self.used -= entry.bytes;
        }
        self.used += bytes;
        self.entries.push_back(MemoryEntry { key, preview, bytes });
    }
    fn remove_paths(&mut self, paths: &HashSet<PathBuf>) {
        self.entries.retain(|entry| {
            if entry.preview.stamps.iter().any(|stamp| paths.contains(&stamp.path)) {
                self.used -= entry.bytes;
                false
            } else { true }
        });
    }
    fn clear(&mut self) { self.entries.clear(); self.used = 0; }
}

struct Control {
    epoch: AtomicU64,
    clearing: AtomicBool,
    pending_writes: AtomicUsize,
    // Command identity is independent of ordinary Browse/preview invalidation.
    last_clear_id: AtomicU64,
}

enum WriterJob {
    Write { epoch: u64, key: String, preview: Preview },
    Touch(String),
    #[cfg(test)]
    FlushForTest(mpsc::Sender<()>),
    Invalidate { paths: Vec<PathBuf> },
    Clear { epoch: u64, reply: mpsc::Sender<Result<(), String>> },
}

/// Shared only by the Info-pane worker and the Browse state. Neither the
/// full-size image viewer nor other image consumers receive this handle.
pub(super) struct InfoPreviewCache {
    memory: Mutex<MemoryLru>,
    control: Arc<Control>,
    writer: mpsc::Sender<WriterJob>,
    path: Option<PathBuf>,
}

impl InfoPreviewCache {
    /// Unit-test binaries default to memory-only previews. Disk-cache tests
    /// provide their own temporary SQLite path through at_path().
    #[cfg(test)]
    pub fn new() -> Self { Self::memory_only() }

    /// Production keeps using the ordinary XDG/platform cache directory.
    /// AppState's explicit test constructors override this in integration
    /// tests, which compile the library without cfg(test).
    #[cfg(not(test))]
    pub fn new() -> Self {
        Self::with_optional_path(cache_file_path())
    }

    pub(super) fn memory_only() -> Self { Self::with_optional_path(None) }

    #[cfg(test)]
    pub(super) fn is_memory_only_for_test(&self) -> bool { self.path.is_none() }

    #[cfg(test)]
    pub(super) fn at_path(path: PathBuf) -> Self { Self::with_optional_path(Some(path)) }

    fn with_optional_path(path: Option<PathBuf>) -> Self {
        let (writer, rx) = mpsc::channel();
        let control = Arc::new(Control {
            epoch: AtomicU64::new(0), clearing: AtomicBool::new(false),
            pending_writes: AtomicUsize::new(0), last_clear_id: AtomicU64::new(0),
        });
        let worker_control = Arc::clone(&control);
        let worker_path = path.clone();
        // No directory, SQLite connection, or image file opened at startup.
        thread::spawn(move || writer_loop(rx, worker_control, worker_path));
        Self { memory: Mutex::new(MemoryLru::default()), control, writer, path }
    }

    pub fn epoch(&self) -> u64 { self.control.epoch.load(Ordering::Acquire) }

    pub fn latest_clear_id(&self) -> u64 { self.control.last_clear_id.load(Ordering::Acquire) }

    /// Browse became hidden or Info artwork was disabled: complete ordinary
    /// source jobs if already running, but reject their future cache writes.
    pub fn cancel_pending(&self) {
        if !self.control.clearing.load(Ordering::Acquire) {
            self.control.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
    fn permits(&self, epoch: u64) -> bool {
        self.epoch() == epoch && !self.control.clearing.load(Ordering::Acquire)
    }

    /// Runs only in the image decode worker; source stamps and disk reads
    /// never block the interactive reducer or the terminal draw pass.
    pub fn lookup(&self, source: &ArtworkSource, epoch: u64) -> Option<(Preview, bool)> {
        if !self.permits(epoch) { return None; }
        let key = source_key(source);
        if let Ok(mut memory) = self.memory.lock() {
            if let Some(entry) = memory.get(&key) {
                drop(memory);
                if matches_source(source, &entry.stamps) && self.permits(epoch) {
                    // Memory-only navigation still counts as persistent LRU use.
                    // The single writer batches the update; the UI does no SQL.
                    let _ = self.writer.send(WriterJob::Touch(key));
                    return Some((entry, true));
                }
                if let Ok(mut memory) = self.memory.lock() { memory.remove(&key); }
            }
        }
        if !self.permits(epoch) { return None; }
        let path = self.path.as_ref()?;
        let entry = read_persistent(path, &key, source)?;
        if !self.permits(epoch) { return None; }
        let _ = self.writer.send(WriterJob::Touch(key));
        Some((entry, true))
    }

    /// Called only after selection-generation validation on the UI reducer.
    /// No encoder work or SQL runs here. Source validation for storage is
    /// repeated by the writer after the optional lossy work is scheduled.
    pub fn admit(&self, source: &ArtworkSource, epoch: u64, preview: Preview, was_hit: bool) {
        if !self.permits(epoch) { return; }
        let key = source_key(source);
        if let Ok(mut memory) = self.memory.lock() { memory.put(key.clone(), preview.clone()); }
        if was_hit || self.path.is_none() || !self.permits(epoch) { return; }
        // A few rapid navigations must not build an unbounded encode queue.
        let pending = self.control.pending_writes.fetch_add(1, Ordering::AcqRel);
        if pending >= MAX_QUEUED_WRITES {
            self.control.pending_writes.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        if self.writer.send(WriterJob::Write { epoch, key, preview }).is_err() {
            self.control.pending_writes.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// On a completed embedded-art write, discard any positive preview that
    /// depends on the changed file (including prior fallback candidates).
    /// This is asynchronous and does not touch authoritative metadata.
    pub fn invalidate_paths(&self, changed: &[PathBuf]) {
        if changed.is_empty() { return; }
        // A pending clear has already invalidated all older work. Do not
        // supersede that clear or leave the cache stuck in clearing mode.
        if !self.control.clearing.load(Ordering::Acquire) {
            self.control.epoch.fetch_add(1, Ordering::AcqRel);
        }
        let paths: HashSet<_> = changed.iter().map(|path| absolute_path(path)).collect();
        if let Ok(mut memory) = self.memory.lock() { memory.remove_paths(&paths); }
        let _ = self.writer.send(WriterJob::Invalidate { paths: paths.into_iter().collect() });
    }

    /// Invalidate all outstanding work immediately. Complete the disk clear
    /// through the serialized writer and report its result asynchronously.
    pub fn clear(&self) -> (u64, mpsc::Receiver<Result<(), String>>) {
        self.control.clearing.store(true, Ordering::Release);
        let epoch = self.control.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        // A later navigation may change `epoch` after this clear completes,
        // but must not suppress its reply. Only another clear supersedes it.
        let clear_id = self.control.last_clear_id.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        if let Ok(mut memory) = self.memory.lock() { memory.clear(); }
        let (reply, rx) = mpsc::channel();
        if self.writer.send(WriterJob::Clear { epoch, reply }).is_err() {
            self.control.clearing.store(false, Ordering::Release);
            // Sender was dropped; report the failure through a live channel.
            let (tx, failed) = mpsc::channel();
            let _ = tx.send(Err("Artwork cache worker unavailable".into()));
            return (clear_id, failed);
        }
        (clear_id, rx)
    }
}

#[cfg(not(test))]
fn cache_file_path() -> Option<PathBuf> {
    let home = std::env::var_os("XDG_CACHE_HOME").filter(|v| Path::new(v).is_absolute())
        .map(PathBuf::from).or_else(dirs::cache_dir)?;
    Some(home.join("tonepoet").join("info-artwork").join("previews.sqlite3"))
}

fn open_read(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(125));
    Some(conn)
}

fn read_persistent(path: &Path, key: &str, source: &ArtworkSource) -> Option<Preview> {
    let conn = open_read(path)?;
    let (stamps_json, blob, width, height): (String, Vec<u8>, u32, u32) = conn.query_row(
        "SELECT stamps, webp, width, height FROM previews WHERE cache_key = ?1 AND version = ?2",
        params![key, FORMAT_VERSION],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional().ok()??;
    if blob.len() > ENTRY_BUDGET || width == 0 || height == 0 || width > PREVIEW_EDGE || height > PREVIEW_EDGE {
        return None;
    }
    // Release SQLite's read transaction before WebP decompression; a clear
    // or eviction in another process must not wait for image decoding.
    drop(conn);
    let stamps: Vec<SourceStamp> = serde_json::from_str(&stamps_json).ok()?;
    if !matches_source(source, &stamps) { return None; }
    let mut reader = ImageReader::with_format(std::io::Cursor::new(blob), ImageFormat::WebP);
    let mut limits = Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits.max_image_width = Some(PREVIEW_EDGE);
    limits.max_image_height = Some(PREVIEW_EDGE);
    reader.limits(limits);
    let pixels = reader.decode().ok()?;
    if pixels.width() != width || pixels.height() != height { return None; }
    let embedded_path = match source {
        ArtworkSource::Embedded(_) => stamps.last().map(|s| s.path.clone()),
        ArtworkSource::File(_) => None,
    };
    Some(Preview { pixels: Arc::new(pixels), embedded_path, stamps })
}

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_millis(300))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")?;
    Ok(())
}

fn initialize_db(conn: &Connection) -> rusqlite::Result<()> {
    // New databases compact freed pages on eviction, rather than accumulating
    // hundreds of MiB of unreferenced WebP BLOB pages over months of use.
    conn.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL;")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != FORMAT_VERSION {
        conn.execute_batch("DROP TABLE IF EXISTS source_paths; DROP TABLE IF EXISTS previews;")?;
    }
    conn.execute_batch("CREATE TABLE IF NOT EXISTS previews (
        cache_key TEXT PRIMARY KEY NOT NULL,
        version INTEGER NOT NULL,
        stamps TEXT NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        payload_bytes INTEGER NOT NULL,
        webp BLOB NOT NULL,
        touched INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS source_paths (
        cache_key TEXT NOT NULL REFERENCES previews(cache_key) ON DELETE CASCADE,
        source_path BLOB NOT NULL,
        PRIMARY KEY (cache_key, source_path)
    );
    CREATE INDEX IF NOT EXISTS source_paths_by_path ON source_paths(source_path);
    PRAGMA user_version=1;")?;
    // The database file itself is bounded, including SQLite page overhead.
    // A saturated cache may skip new entries until eviction frees pages.
    let _: i64 = conn.query_row("PRAGMA max_page_count=65536", [], |r| r.get(0))?;
    Ok(())
}

fn open_write(path: &Path) -> Result<Connection, String> {
    let parent = path.parent().ok_or("Invalid cache path")?;
    if fs::symlink_metadata(parent).is_ok_and(|md| md.file_type().is_symlink())
        || fs::symlink_metadata(path).is_ok_and(|md| md.file_type().is_symlink()) {
        return Err("Artwork cache refuses symbolic-link database or directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(|e| e.to_string())?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Recover only an actual corrupt/not-a-database condition. Never remove
    // a live database because of a transient lock or permission failure.
    let open_once = || -> rusqlite::Result<Connection> {
        let conn = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        configure_connection(&conn)?;
        initialize_db(&conn)?;
        Ok(conn)
    };
    match open_once() {
        Ok(conn) => Ok(conn),
        Err(error) if is_corrupt(&error) => {
            fs::remove_file(path).map_err(|e| format!("Cannot reset damaged artwork cache: {e}"))?;
            open_once().map_err(|e| e.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn is_corrupt(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _)
        if matches!(code.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
}

fn now_s() -> i64 {
    // Microsecond ordering avoids accidental alphabetical eviction when many
    // albums are visited in the same second.
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
        .as_micros().min(i64::MAX as u128) as i64
}

fn persist(conn: &mut Connection, key: &str, preview: &Preview) -> Result<(), String> {
    if preview.pixels.width() > PREVIEW_EDGE || preview.pixels.height() > PREVIEW_EDGE
        || preview.pixels.width() == 0 || preview.pixels.height() == 0 { return Ok(()); }
    let rgba = preview.pixels.to_rgba8();
    let mut blob = Vec::new();
    WebPEncoder::new_lossless(&mut blob).write_image(
        rgba.as_raw(), preview.pixels.width(), preview.pixels.height(), image::ExtendedColorType::Rgba8,
    ).map_err(|e| format!("WebP encoding failed: {e}"))?;
    if blob.len() > ENTRY_BUDGET { return Ok(()); }
    let stamps = serde_json::to_string(&preview.stamps).map_err(|e| e.to_string())?;
    persist_with_limit(conn, key, preview, blob, stamps, DISK_BUDGET - 4 * ENTRY_BUDGET as i64)
}

fn persist_with_limit(conn: &mut Connection, key: &str, preview: &Preview,
                      blob: Vec<u8>, stamps: String, disk_limit: i64) -> Result<(), String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    // Evict BEFORE inserting, so a full SQLite file cannot permanently
    // prevent replacement despite many evictable old entries.
    let mut total: i64 = tx.query_row("SELECT COALESCE(SUM(payload_bytes),0) FROM previews", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let previous: i64 = tx.query_row("SELECT payload_bytes FROM previews WHERE cache_key = ?1",
        [key], |r| r.get(0)).optional().map_err(|e| e.to_string())?.unwrap_or(0);
    while total - previous + blob.len() as i64 > disk_limit {
        let (old_key, bytes): (String, i64) = tx.query_row(
            "SELECT cache_key, payload_bytes FROM previews WHERE cache_key != ?1
                ORDER BY touched ASC, cache_key ASC LIMIT 1",
            [key], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM previews WHERE cache_key = ?1", [&old_key]).map_err(|e| e.to_string())?;
        total -= bytes;
    }
    tx.execute("INSERT OR REPLACE INTO previews
        (cache_key, version, stamps, width, height, payload_bytes, webp, touched)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![key, FORMAT_VERSION, stamps, preview.pixels.width(), preview.pixels.height(),
            blob.len() as i64, blob, now_s()]).map_err(|e| e.to_string())?;
    for candidate in &preview.stamps {
        tx.execute("INSERT OR REPLACE INTO source_paths (cache_key, source_path) VALUES (?1, ?2)",
            params![key, candidate.path.as_os_str().as_encoded_bytes()])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_paths(conn: &mut Connection, paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() { return Ok(()); }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for path in paths {
        tx.execute("DELETE FROM previews WHERE cache_key IN
            (SELECT cache_key FROM source_paths WHERE source_path = ?1)",
            [path.as_os_str().as_encoded_bytes()]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn clear_disk(conn: &mut Connection) -> Result<(), String> {
    conn.execute("DELETE FROM previews", []).map_err(|e| e.to_string())?;
    // VACUUM is deliberately never executed on the UI thread. All writer
    // operations use the same connection and DELETE journaling, not WAL.
    conn.execute_batch("VACUUM").map_err(|e| e.to_string())
}

// Retain recency order within a batch. A repeat moves the key to the end;
// at most 128 keys are pending, so the linear deduplication is inexpensive.
fn queue_touch(touched: &mut VecDeque<String>, key: String) {
    touched.retain(|previous| previous != &key);
    touched.push_back(key);
}

fn flush_accesses(connection: &mut Option<Connection>, touched: &mut VecDeque<String>) {
    if touched.is_empty() { return; }
    if let Some(conn) = connection.as_mut() {
        if let Ok(tx) = conn.transaction() {
            // Assign distinct ticks even when a whole batch fits in one
            // clock microsecond. Processing order is the navigation order.
            let mut tick = now_s();
            for key in touched.drain(..) {
                let _ = tx.execute("UPDATE previews SET touched = ?1 WHERE cache_key = ?2",
                    params![tick, key]);
                tick = tick.saturating_add(1);
            }
            let _ = tx.commit();
        }
    } else { touched.clear(); }
}

fn writer_loop(rx: mpsc::Receiver<WriterJob>, control: Arc<Control>, path: Option<PathBuf>) {
    let mut connection: Option<Connection> = None;
    let mut touched = VecDeque::new();
    loop {
        let job = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                flush_accesses(&mut connection, &mut touched);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match job {
            WriterJob::Touch(key) => {
                if connection.is_none() {
                    connection = path.as_ref().filter(|p| p.exists())
                        .and_then(|p| open_write(p).ok());
                }
                queue_touch(&mut touched, key);
                if touched.len() >= 128 { flush_accesses(&mut connection, &mut touched); }
            }
            WriterJob::Write { epoch, key, preview } => {
                let permitted = || control.epoch.load(Ordering::Acquire) == epoch
                    && !control.clearing.load(Ordering::Acquire);
                if permitted() && matches_source_path_stamps(&preview.stamps) {
                    if connection.is_none() {
                        connection = path.as_ref().and_then(|p| open_write(p).ok());
                    }
                    // A pending revisit must be durable before the eviction
                    // query in persist_with_limit examines LRU order.
                    flush_accesses(&mut connection, &mut touched);
                    if permitted() {
                        if let Some(conn) = connection.as_mut() {
                            if let Err(error) = persist(conn, &key, &preview) {
                                log::debug!("Info artwork cache write skipped: {error}");
                            }
                        }
                    }
                }
                control.pending_writes.fetch_sub(1, Ordering::AcqRel);
            }
            #[cfg(test)]
            WriterJob::FlushForTest(reply) => {
                flush_accesses(&mut connection, &mut touched);
                let _ = reply.send(());
            }
            WriterJob::Invalidate { paths } => {
                if connection.is_none() {
                    connection = path.as_ref().filter(|p| p.exists())
                        .and_then(|p| open_write(p).ok());
                }
                if let Some(conn) = connection.as_mut() { let _ = delete_paths(conn, &paths); }
            }
            WriterJob::Clear { epoch, reply } => {
                touched.clear();
                let result = if control.epoch.load(Ordering::Acquire) != epoch {
                    Err("Superseded by a newer artwork cache operation".into())
                } else if path.as_ref().is_none_or(|p| !p.exists()) {
                    Ok(())
                } else {
                    if connection.is_none() { connection = path.as_ref().and_then(|p| open_write(p).ok()); }
                    match connection.as_mut() {
                        Some(conn) => clear_disk(conn),
                        None => Err("Unable to open artwork cache database for clearing".into()),
                    }
                };
                if control.epoch.load(Ordering::Acquire) == epoch {
                    control.clearing.store(false, Ordering::Release);
                }
                let _ = reply.send(result);
            }
        }
    }
    flush_accesses(&mut connection, &mut touched);
}

fn matches_source_path_stamps(stamps: &[SourceStamp]) -> bool {
    !stamps.is_empty() && stamps.iter().all(|expected| stamp(&expected.path).as_ref() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_unit_test_cache_cannot_write_to_the_user_cache_directory() {
        let cache = InfoPreviewCache::new();
        assert!(cache.path.is_none(), "unit tests must opt in to disk cache using at_path");
        let (id, done) = cache.clear();
        assert_eq!(cache.latest_clear_id(), id);
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    }

    fn file_source(path: &Path) -> ArtworkSource { ArtworkSource::File(path.to_path_buf()) }
    fn preview(path: &Path, value: u8) -> Preview {
        Preview {
            pixels: Arc::new(DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(32, 32,
                image::Rgba([value, value, value, 255])))),
            embedded_path: None,
            stamps: stamps_for_decoded(&file_source(path), None).unwrap(),
        }
    }
    fn await_disk(path: &Path, source: &ArtworkSource) {
        for _ in 0..200 {
            if read_persistent(path, &source_key(source), source).is_some() { return; }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for persistent write");
    }

    fn sync_writer(cache: &InfoPreviewCache) {
        let (tx, rx) = mpsc::channel();
        cache.writer.send(WriterJob::FlushForTest(tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(30)).expect("writer did not flush");
    }

    #[test]
    fn memory_hit_updates_persistent_recency_without_sql_in_lookup() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("previews.sqlite3");
        let file = root.path().join("cover.png");
        fs::write(&file, b"original").unwrap();
        let source = file_source(&file);
        let key = source_key(&source);
        let cache = InfoPreviewCache::at_path(db.clone());
        cache.admit(&source, cache.epoch(), preview(&file, 7), false);
        sync_writer(&cache);
        let before: i64 = open_read(&db).unwrap().query_row(
            "SELECT touched FROM previews WHERE cache_key=?1", [&key], |row| row.get(0)).unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(cache.lookup(&source, cache.epoch()).is_some());
        sync_writer(&cache);
        let after: i64 = open_read(&db).unwrap().query_row(
            "SELECT touched FROM previews WHERE cache_key=?1", [&key], |row| row.get(0)).unwrap();
        assert!(after > before, "memory reuse must refresh the disk LRU");
    }

    #[test]
    fn queued_touch_is_applied_before_budget_eviction() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("previews.sqlite3");
        let conn = open_write(&db).unwrap();
        let mut connection = Some(conn);
        let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([52, 91, 113, 255]));
        let mut blob = Vec::new();
        WebPEncoder::new_lossless(&mut blob).write_image(image.as_raw(),
            32, 32, image::ExtendedColorType::Rgba8).unwrap();
        let limit = blob.len() as i64 * 2 + 5;
        let mut items = Vec::new();
        for i in 0..3 {
            let path = root.path().join(format!("{i}.png"));
            fs::write(&path, [i as u8]).unwrap();
            let source = file_source(&path);
            let preview = Preview { pixels: Arc::new(DynamicImage::ImageRgba8(image.clone())),
                embedded_path: None, stamps: stamps_for_decoded(&source, None).unwrap() };
            items.push((source_key(&source), source, preview));
        }
        for (key, _, preview) in &items[..2] {
            persist_with_limit(connection.as_mut().unwrap(), key, preview,
                blob.clone(), serde_json::to_string(&preview.stamps).unwrap(), limit).unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        // A was older than B, then A was revisited. Simulate the actual
        // writer's pending batch and its flush-before-persist ordering.
        let mut touches = VecDeque::new();
        queue_touch(&mut touches, items[0].0.clone());
        flush_accesses(&mut connection, &mut touches);
        let (key_c, _, preview_c) = &items[2];
        persist_with_limit(connection.as_mut().unwrap(), key_c, preview_c,
            blob, serde_json::to_string(&preview_c.stamps).unwrap(), limit).unwrap();
        let conn = connection.as_ref().unwrap();
        let present = |key: &str| -> bool { conn.query_row(
            "SELECT 1 FROM previews WHERE cache_key=?1", [key], |_| Ok(())).is_ok() };
        assert!(present(&items[0].0), "recently used A must remain");
        assert!(!present(&items[1].0), "older B should be evicted");
        assert!(present(&items[2].0), "new C must be admitted");
    }

    #[test]
    fn batched_touches_keep_last_visit_order() {
        let mut touches = VecDeque::new();
        for key in ["a", "b", "a", "c", "b"] { queue_touch(&mut touches, key.into()); }
        assert_eq!(touches.into_iter().collect::<Vec<_>>(),
            vec!["a".to_owned(), "c".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn memory_lru_reorders_evicts_and_counts_bytes() {
        let mut lru = MemoryLru::default();
        let image = |size| Preview { pixels: Arc::new(DynamicImage::new_rgba8(size, size)),
            embedded_path: None, stamps: Vec::new() };
        lru.put("a".into(), image(2047));
        lru.put("b".into(), image(1024));
        assert!(lru.used <= MEMORY_BUDGET);
        assert!(lru.get("a").is_some());
        lru.put("c".into(), image(2047));
        assert!(lru.get("b").is_none());
        assert!(lru.get("a").is_some());
        assert!(lru.get("c").is_some());
        assert!(lru.used <= MEMORY_BUDGET);
    }

    #[test]
    fn memory_accounting_includes_high_depth_pixel_storage() {
        let pixels = DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
            1024, 1024, image::Rgba([1_u16, 2, 3, 4])));
        let expected = pixels.as_bytes().len();
        assert_eq!(expected, 8 * 1024 * 1024);
        let entry = Preview { pixels: Arc::new(pixels), stamps: Vec::new(), embedded_path: None };
        assert_eq!(entry.memory_bytes(), expected);
    }

    #[test]
    fn lossless_webp_roundtrip_fidelity_disk_hit_and_clear_reclaim() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("previews.sqlite3");
        let source_path = root.path().join("cover.png");
        fs::write(&source_path, b"original source contents").unwrap();
        let source = file_source(&source_path);
        let cache = InfoPreviewCache::at_path(path.clone());
        let first = preview(&source_path, 114);
        cache.admit(&source, cache.epoch(), first.clone(), false);
        await_disk(&path, &source);
        let loaded = read_persistent(&path, &source_key(&source), &source).unwrap();
        assert_eq!(loaded.pixels.to_rgba8(), first.pixels.to_rgba8());
        assert!(cache.lookup(&source, cache.epoch()).is_some());
        let (clear_id, done) = cache.clear();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        assert_eq!(cache.latest_clear_id(), clear_id);
        assert!(cache.lookup(&source, cache.epoch()).is_none());
        assert!(fs::metadata(&path).unwrap().len() < 128 * 1024);
    }

    #[test]
    fn replacing_source_invalidates_memory_and_disk() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("previews.sqlite3");
        let file = root.path().join("cover.png");
        fs::write(&file, b"first image").unwrap();
        let source = file_source(&file);
        let cache = InfoPreviewCache::at_path(db.clone());
        cache.admit(&source, cache.epoch(), preview(&file, 10), false);
        await_disk(&db, &source);
        fs::write(&file, b"second image larger than first").unwrap();
        assert!(cache.lookup(&source, cache.epoch()).is_none());
        cache.invalidate_paths(&[file]);
    }

    #[test]
    fn embedded_fallback_includes_earlier_candidates_in_validity() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("01.flac");
        let second = root.path().join("02.flac");
        fs::write(&first, b"no artwork").unwrap();
        fs::write(&second, b"has artwork").unwrap();
        let source = ArtworkSource::Embedded(Arc::from(vec![first.clone(), second.clone()]));
        let stamps = stamps_for_decoded(&source, Some(&second)).unwrap();
        assert!(matches_source(&source, &stamps));
        fs::write(&first, b"new embedded cover added to higher-priority track").unwrap();
        assert!(!matches_source(&source, &stamps));
    }

    #[test]
    fn clear_rejects_queued_old_writes_without_stopping_source_display() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("cover.jpg");
        fs::write(&file, b"source").unwrap();
        let cache = InfoPreviewCache::at_path(root.path().join("previews.sqlite3"));
        let source = file_source(&file);
        let old = cache.epoch();
        let (_new, done) = cache.clear();
        cache.admit(&source, old, preview(&file, 20), false);
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        assert!(cache.lookup(&source, cache.epoch()).is_none());
    }


    #[test]
    fn corrupt_database_is_replaced_without_touching_source() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("cover.jpg");
        fs::write(&file, b"real source bytes").unwrap();
        let db = root.path().join("previews.sqlite3");
        fs::write(&db, b"this is not a sqlite database").unwrap();
        let cache = InfoPreviewCache::at_path(db.clone());
        let (clear_id, done) = cache.clear();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        assert_eq!(cache.latest_clear_id(), clear_id);
        assert_eq!(fs::read(&file).unwrap(), b"real source bytes");
        assert!(Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap().query_row("SELECT COUNT(*) FROM previews", [], |r| r.get::<_, i64>(0)).is_ok());
    }

    #[test]
    fn disk_budget_evicts_lru_before_inserting_and_limits_individual_blobs() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("previews.sqlite3");
        let mut conn = open_write(&db).unwrap();
        let mut image = image::RgbaImage::new(64, 64);
        for (idx, pixel) in image.pixels_mut().enumerate() {
            let value = ((idx as u32).wrapping_mul(27183) ^ 0x379ea81) as u8;
            *pixel = image::Rgba([value, 255 - value, value.wrapping_add(51), 255]);
        }
        let mut blob = Vec::new();
        WebPEncoder::new_lossless(&mut blob).write_image(
            image.as_raw(), 64, 64, image::ExtendedColorType::Rgba8).unwrap();
        assert!(blob.len() < ENTRY_BUDGET);
        let entry_bytes = blob.len() as i64;
        let mut names = Vec::new();
        for i in 0..3 {
            let file = root.path().join(format!("cover{i}.png"));
            fs::write(&file, [i as u8; 8]).unwrap();
            let source = file_source(&file);
            let entry = Preview { pixels: Arc::new(DynamicImage::ImageRgba8(image.clone())),
                stamps: stamps_for_decoded(&source, None).unwrap(), embedded_path: None };
            let key = source_key(&source);
            let stamps = serde_json::to_string(&entry.stamps).unwrap();
            persist_with_limit(&mut conn, &key, &entry, blob.clone(), stamps,
                entry_bytes * 2 + 10).unwrap();
            names.push((key, source));
            thread::sleep(Duration::from_millis(2));
        }
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM previews", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        assert!(read_persistent(&db, &names[0].0, &names[0].1).is_none());
        assert!(read_persistent(&db, &names[2].0, &names[2].1).is_some());
    }

    #[test]
    fn oversized_lossless_webp_stays_in_memory_and_bypasses_sqlite() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("photo.png");
        fs::write(&source_path, b"source, never modified").unwrap();
        // SplitMix64 gives reproducible high-entropy *RGBA* bytes. Using an
        // opaque alpha plane would leave only 1.69 MiB of random RGB data,
        // which could not reliably exceed a 2 MiB compressed-payload cap.
        let mut state = 0x6a09_e667_f3bc_c909_u64;
        let image = image::RgbaImage::from_fn(PREVIEW_EDGE, PREVIEW_EDGE, |_, _| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            let bytes = (value ^ (value >> 31)).to_le_bytes();
            image::Rgba([bytes[0], bytes[1], bytes[2], bytes[3]])
        });
        let mut encoded = Vec::new();
        WebPEncoder::new_lossless(&mut encoded).write_image(image.as_raw(),
            PREVIEW_EDGE, PREVIEW_EDGE, image::ExtendedColorType::Rgba8).unwrap();
        assert!(encoded.len() > ENTRY_BUDGET, "fixture must exceed the per-entry cap");
        let db = root.path().join("previews.sqlite3");
        let source = file_source(&source_path);
        let preview = Preview { pixels: Arc::new(DynamicImage::ImageRgba8(image)),
            embedded_path: None, stamps: stamps_for_decoded(&source, None).unwrap() };
        let original_pixels = Arc::clone(&preview.pixels);
        let cache = InfoPreviewCache::at_path(db.clone());
        cache.admit(&source, cache.epoch(), preview, false);
        // Oversized disk payloads must not disqualify the decoded preview.
        let (hit, _) = cache.lookup(&source, cache.epoch()).expect("memory display remains available");
        assert!(Arc::ptr_eq(&hit.pixels, &original_pixels));
        // Wait for the *real writer* to finish encoding and reject the blob.
        sync_writer(&cache);
        assert!(read_persistent(&db, &source_key(&source), &source).is_none());
        assert_eq!(fs::read(source_path).unwrap(), b"source, never modified");
    }

    #[test]
    fn source_mutation_purges_only_dependent_entries() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("previews.sqlite3");
        let cache = InfoPreviewCache::at_path(db.clone());
        let a = root.path().join("a.jpg");
        let b = root.path().join("b.jpg");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let source_a = file_source(&a);
        let source_b = file_source(&b);
        cache.admit(&source_a, cache.epoch(), preview(&a, 50), false);
        cache.admit(&source_b, cache.epoch(), preview(&b, 70), false);
        await_disk(&db, &source_a);
        await_disk(&db, &source_b);
        cache.invalidate_paths(&[a]);
        // The invalidation job is serialized behind the earlier writes.
        for _ in 0..100 {
            if read_persistent(&db, &source_key(&source_a), &source_a).is_none() { break; }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(read_persistent(&db, &source_key(&source_a), &source_a).is_none());
        assert!(read_persistent(&db, &source_key(&source_b), &source_b).is_some());
        let (_epoch, done) = cache.clear();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        // All entries removed by explicit clear, including the otherwise
        // independent entry; targeted deletion was exercised before clear.
        assert!(cache.lookup(&source_b, cache.epoch()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_database_is_never_followed_or_modified() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let authoritative = root.path().join("metadata.sqlite3");
        fs::write(&authoritative, b"authoritative contents").unwrap();
        let cache_file = root.path().join("previews.sqlite3");
        symlink(&authoritative, &cache_file).unwrap();
        assert!(open_write(&cache_file).is_err());
        let cache = InfoPreviewCache::at_path(cache_file);
        let (clear_id, done) = cache.clear();
        let failure = done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
        assert!(failure.contains("Unable to open"), "unexpected clear failure: {failure}");
        assert_eq!(cache.latest_clear_id(), clear_id);
        assert_eq!(fs::read(authoritative).unwrap(), b"authoritative contents");
    }

    #[test]
    fn read_only_unwritable_or_missing_cache_never_blocks_pixel_decoding() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("cover.png");
        fs::write(&file, b"pixels").unwrap();
        let source = file_source(&file);
        let cache = InfoPreviewCache::at_path(root.path().join("unavailable").join("previews.sqlite3"));
        assert!(cache.lookup(&source, cache.epoch()).is_none());
        let pixels = preview(&file, 99);
        cache.admit(&source, cache.epoch(), pixels.clone(), false);
        assert_eq!(cache.lookup(&source, cache.epoch()).unwrap().0.pixels.width(), 32);
        // A path occupied by a regular file is permanently unwritable as a directory.
        let bad_dir = root.path().join("regular-file");
        fs::write(&bad_dir, b"not a directory").unwrap();
        let bad = InfoPreviewCache::at_path(bad_dir.join("previews.sqlite3"));
        bad.admit(&source, bad.epoch(), pixels, false);
        assert!(bad.lookup(&source, bad.epoch()).is_some());
    }
}
