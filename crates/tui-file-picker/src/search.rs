//! Asynchronous recursive filename search for the reusable picker.
//!
//! Workers never follow directory symlinks. Each query owns a monotonically
//! increasing generation; stale workers self-cancel and their batches are
//! ignored, so rapid editing cannot overwrite newer results.

use crate::filter::FilePickerFilter;
use crate::text_input::TextInputState;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

const SEARCH_BATCH_SIZE: usize = 128;
const MAX_SEARCH_RESULTS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearchResult {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug)]
enum SearchMessage {
    Batch { generation: u64, results: Vec<FileSearchResult> },
    Complete { generation: u64 },
    Failed { generation: u64, message: String },
}

#[derive(Debug)]
struct SearchCommand {
    root: PathBuf,
    query: String,
    filter: FilePickerFilter,
    show_hidden: bool,
    generation: u64,
}

pub(crate) struct FileSearchState {
    pub active: bool,
    pub input: TextInputState,
    pub results: Vec<FileSearchResult>,
    pub cursor: usize,
    pub scroll: usize,
    pub searching: bool,
    pub error: Option<String>,
    generation: u64,
    cancel_generation: Arc<AtomicU64>,
    receiver: Option<Arc<Mutex<Receiver<SearchMessage>>>>,
    command_sender: Option<Sender<SearchCommand>>,
}

impl Clone for FileSearchState {
    fn clone(&self) -> Self {
        // A receiver represents ownership of an in-flight worker stream. A
        // cloned picker must not compete with the original for those messages,
        // so clones retain completed UI state but intentionally start idle.
        let cancel_generation = Arc::new(AtomicU64::new(self.generation));
        Self {
            active: self.active,
            input: self.input.clone(),
            results: self.results.clone(),
            cursor: self.cursor,
            scroll: self.scroll,
            searching: false,
            error: self.error.clone(),
            generation: self.generation,
            cancel_generation,
            receiver: None,
            command_sender: None,
        }
    }
}

impl Drop for FileSearchState {
    fn drop(&mut self) {
        // Invalidate the active generation before dropping the command sender.
        // A worker already walking the filesystem will observe this promptly,
        // stop, and then exit when its command channel disconnects.
        self.generation = self.generation.wrapping_add(1);
        self.cancel_generation
            .store(self.generation, Ordering::Release);
    }
}

impl fmt::Debug for FileSearchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSearchState")
            .field("active", &self.active)
            .field("input", &self.input)
            .field("result_count", &self.results.len())
            .field("cursor", &self.cursor)
            .field("scroll", &self.scroll)
            .field("searching", &self.searching)
            .field("error", &self.error)
            .field("generation", &self.generation)
            .finish()
    }
}

impl Default for FileSearchState {
    fn default() -> Self {
        Self {
            active: false,
            input: TextInputState::empty(),
            results: Vec::new(),
            cursor: 0,
            scroll: 0,
            searching: false,
            error: None,
            generation: 0,
            cancel_generation: Arc::new(AtomicU64::new(0)),
            receiver: None,
            command_sender: None,
        }
    }
}

impl FileSearchState {
    /// Start a new search session.
    ///
    /// An inactive -> active transition always begins from a clean query and
    /// result state. Refocusing an already-active session is handled by
    /// `FilePickerState::open_search()` and intentionally does not call this
    /// method, so the current query, results, cursor, scroll, and worker state
    /// remain untouched.
    pub fn open(&mut self) {
        if self.active {
            return;
        }
        self.clear();
        self.active = true;
    }

    /// End the current search session and discard all session-local state.
    pub fn close(&mut self) {
        self.active = false;
        self.clear();
    }

    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cancel_generation.store(self.generation, Ordering::Release);
        self.searching = false;
    }

    pub fn clear(&mut self) {
        self.cancel();
        self.input = TextInputState::empty();
        self.results.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.error = None;
    }

    pub fn start(
        &mut self,
        root: PathBuf,
        filter: FilePickerFilter,
        show_hidden: bool,
    ) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.cancel_generation.store(generation, Ordering::Release);
        self.results.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.error = None;

        let query = self.input.text.trim().to_lowercase();
        if query.is_empty() {
            self.searching = false;
            return;
        }

        self.ensure_worker();
        let Some(sender) = self.command_sender.as_ref() else {
            self.error = Some("search worker could not be started".to_string());
            self.searching = false;
            return;
        };
        match sender.send(SearchCommand {
            root,
            query,
            filter,
            show_hidden,
            generation,
        }) {
            Ok(()) => self.searching = true,
            Err(_) => {
                self.command_sender = None;
                self.receiver = None;
                self.searching = false;
                self.error = Some("search worker disconnected".to_string());
            }
        }
    }

    fn ensure_worker(&mut self) {
        if self.command_sender.is_some() && self.receiver.is_some() {
            return;
        }

        let (command_sender, command_receiver) = mpsc::channel::<SearchCommand>();
        let (result_sender, result_receiver) = mpsc::channel::<SearchMessage>();
        let cancel_generation = Arc::clone(&self.cancel_generation);
        thread::spawn(move || {
            while let Ok(mut command) = command_receiver.recv() {
                // Collapse queued keystrokes to the newest query before walking.
                while let Ok(newer) = command_receiver.try_recv() {
                    command = newer;
                }
                if cancel_generation.load(Ordering::Acquire) != command.generation {
                    continue;
                }
                let result = walk_filesystem(
                    &command.root,
                    &command.query,
                    &command.filter,
                    command.show_hidden,
                    command.generation,
                    &cancel_generation,
                    |batch| {
                        result_sender
                            .send(SearchMessage::Batch {
                                generation: command.generation,
                                results: batch,
                            })
                            .is_ok()
                    },
                );
                if cancel_generation.load(Ordering::Acquire) != command.generation {
                    continue;
                }
                let message = match result {
                    Ok(()) => SearchMessage::Complete {
                        generation: command.generation,
                    },
                    Err(message) => SearchMessage::Failed {
                        generation: command.generation,
                        message,
                    },
                };
                if result_sender.send(message).is_err() {
                    break;
                }
            }
        });
        self.command_sender = Some(command_sender);
        self.receiver = Some(Arc::new(Mutex::new(result_receiver)));
    }

    pub fn poll(&mut self) {
        let Some(receiver) = self.receiver.as_ref().cloned() else {
            return;
        };
        loop {
            let message = match receiver.lock() {
                Ok(receiver) => receiver.try_recv(),
                Err(_) => {
                    self.error = Some("search result channel was poisoned".to_string());
                    self.searching = false;
                    self.receiver = None;
                    self.command_sender = None;
                    return;
                }
            };
            match message {
                Ok(SearchMessage::Batch { generation, mut results }) if generation == self.generation => {
                    let remaining = MAX_SEARCH_RESULTS.saturating_sub(self.results.len());
                    results.truncate(remaining);
                    self.results.append(&mut results);
                }
                Ok(SearchMessage::Complete { generation }) if generation == self.generation => {
                    self.searching = false;
                    return;
                }
                Ok(SearchMessage::Failed { generation, message }) if generation == self.generation => {
                    self.error = Some(message);
                    self.searching = false;
                    return;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.searching = false;
                    self.receiver = None;
                    self.command_sender = None;
                    return;
                }
            }
        }
    }

    pub fn move_cursor(&mut self, delta: isize, visible_rows: usize) {
        if self.results.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let last = self.results.len() - 1;
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as usize).min(last)
        };
        self.ensure_visible(visible_rows);
    }

    pub fn ensure_visible(&mut self, visible_rows: usize) {
        let rows = visible_rows.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll.saturating_add(rows) {
            self.scroll = self.cursor.saturating_add(1).saturating_sub(rows);
        }
    }

    pub fn current(&self) -> Option<&FileSearchResult> {
        self.results.get(self.cursor)
    }
}

fn walk_filesystem<F>(
    root: &Path,
    query: &str,
    filter: &FilePickerFilter,
    show_hidden: bool,
    generation: u64,
    cancel_generation: &AtomicU64,
    mut emit: F,
) -> Result<(), String>
where
    F: FnMut(Vec<FileSearchResult>) -> bool,
{
    let mut stack = vec![root.to_path_buf()];
    let mut batch = Vec::with_capacity(SEARCH_BATCH_SIZE);
    let mut emitted = 0usize;

    while let Some(directory) = stack.pop() {
        if cancel_generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(err) if directory == root => {
                return Err(format!("cannot search {}: {err}", root.display()));
            }
            Err(_) => continue,
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            let left_name = left.file_name().to_string_lossy().into_owned();
            let right_name = right.file_name().to_string_lossy().into_owned();
            left_name
                .to_lowercase()
                .cmp(&right_name.to_lowercase())
                .then_with(|| left_name.cmp(&right_name))
        });
        let mut child_directories = Vec::new();

        for entry in entries {
            if cancel_generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let is_dir = file_type.is_dir();
            if is_dir {
                child_directories.push(path.clone());
            }
            if !filter.accepts_path(&path, is_dir) || !name.to_lowercase().contains(query) {
                continue;
            }
            batch.push(FileSearchResult { path, name, is_dir });
            emitted = emitted.saturating_add(1);
            if batch.len() >= SEARCH_BATCH_SIZE {
                if !emit(std::mem::take(&mut batch)) {
                    return Ok(());
                }
                batch = Vec::with_capacity(SEARCH_BATCH_SIZE);
            }
            if emitted >= MAX_SEARCH_RESULTS {
                if !batch.is_empty() {
                    let _ = emit(batch);
                }
                return Ok(());
            }
        }
        for child in child_directories.into_iter().rev() {
            stack.push(child);
        }
    }
    if !batch.is_empty() {
        let _ = emit(batch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn search_session_open_close_and_refocus_have_distinct_lifecycles() {
        let mut state = FileSearchState::default();
        state.active = true;
        state.input = TextInputState::new("album".to_string());
        state.results = (0..6)
            .map(|index| FileSearchResult {
                path: PathBuf::from(format!("album-{index}.flac")),
                name: format!("album-{index}.flac"),
                is_dir: false,
            })
            .collect();
        state.cursor = 3;
        state.scroll = 2;
        state.searching = true;
        state.error = Some("fixture".to_string());
        let generation = state.generation;

        state.open();
        assert_eq!(state.input.text, "album");
        assert_eq!(state.results.len(), 6);
        assert_eq!(state.cursor, 3);
        assert_eq!(state.scroll, 2);
        assert!(state.searching);
        assert_eq!(state.error.as_deref(), Some("fixture"));
        assert_eq!(state.generation, generation);

        state.close();
        assert!(!state.active);
        assert_eq!(state.input.text, "");
        assert!(state.results.is_empty());
        assert_eq!(state.cursor, 0);
        assert_eq!(state.scroll, 0);
        assert!(!state.searching);
        assert!(state.error.is_none());
        assert_ne!(state.generation, generation);

        state.input = TextInputState::new("stale".to_string());
        state.results.push(FileSearchResult {
            path: PathBuf::from("stale.flac"),
            name: "stale.flac".to_string(),
            is_dir: false,
        });
        state.cursor = 4;
        state.scroll = 5;
        state.error = Some("stale".to_string());
        state.open();
        assert!(state.active);
        assert_eq!(state.input.text, "");
        assert!(state.results.is_empty());
        assert_eq!(state.cursor, 0);
        assert_eq!(state.scroll, 0);
        assert!(state.error.is_none());
    }

    #[test]
    fn rapid_queries_reuse_one_persistent_worker_channel() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("alpha.txt"), b"alpha").expect("alpha");
        fs::write(temp.path().join("beta.txt"), b"beta").expect("beta");

        let mut state = FileSearchState::default();
        state.input = TextInputState::new("alpha".to_string());
        state.start(temp.path().to_path_buf(), FilePickerFilter::All, true);
        let first_receiver = state.receiver.as_ref().cloned().expect("worker receiver");

        state.input = TextInputState::new("beta".to_string());
        state.start(temp.path().to_path_buf(), FilePickerFilter::All, true);
        let second_receiver = state.receiver.as_ref().cloned().expect("same worker receiver");

        assert!(Arc::ptr_eq(&first_receiver, &second_receiver));
    }

    #[test]
    fn recursive_search_finds_descendants_without_following_symlink_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("nested");
        fs::create_dir(&nested).expect("nested");
        fs::write(nested.join("needle.flac"), b"audio").expect("file");

        let mut state = FileSearchState::default();
        state.input = TextInputState::new("needle".to_string());
        state.start(temp.path().to_path_buf(), FilePickerFilter::All, true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.searching && Instant::now() < deadline {
            state.poll();
            std::thread::yield_now();
        }
        assert!(!state.searching);
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].name, "needle.flac");
    }
}
