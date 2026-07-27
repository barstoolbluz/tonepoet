//! Process-global test isolation for configuration-path tests.
//!
//! `XDG_CONFIG_HOME` is process-global, while Rust's test harness runs modules
//! concurrently. Every test that overrides it must share this guard; a
//! module-local mutex still permits theme, config, and bookmark tests to race
//! one another and intermittently resolve the live user's configuration path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};

fn xdg_config_home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn test_config_home_cell() -> &'static RwLock<Option<PathBuf>> {
    static OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| RwLock::new(None))
}

/// Explicit test seam used by path helpers that must not rely on a library's
/// environment-variable caching behavior. The suite-wide guard owns writes;
/// production builds do not compile this module.
pub(crate) fn test_config_home_override() -> Option<PathBuf> {
    test_config_home_cell()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Suite-wide, panic-safe owner of an isolated `XDG_CONFIG_HOME`.
///
/// The lock remains held while the environment variable is overridden and is
/// released only after `Drop` restores the exact previous value. Poison is
/// deliberately recovered: a panicking test still restores its environment,
/// and a prior test panic must not disable isolation for the rest of the suite.
pub(crate) struct XdgConfigHomeGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
    /// `XDG_DATA_HOME` is redirected alongside the config home: the metadata
    /// journal database resolves through `dirs::data_dir()`, and a guard that
    /// left it pointing at the user's real data directory let tests write
    /// journal entries into the live tonepoet.db (field-observed leak).
    previous_data: Option<OsString>,
    previous_override: Option<PathBuf>,
    previous_picker_override: Option<PathBuf>,
    directory: tempfile::TempDir,
}

impl XdgConfigHomeGuard {
    pub(crate) fn new(prefix: &str) -> Self {
        let lock = xdg_config_home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create isolated XDG_CONFIG_HOME");
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let previous_data = std::env::var_os("XDG_DATA_HOME");
        let previous_override = {
            let mut override_path = test_config_home_cell()
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            override_path.replace(directory.path().to_path_buf())
        };
        let previous_picker_override =
            tui_file_picker::replace_bookmark_config_home_override_for_tests(Some(
                directory.path().to_path_buf(),
            ));
        std::env::set_var("XDG_CONFIG_HOME", directory.path());
        std::env::set_var("XDG_DATA_HOME", directory.path().join("data"));
        Self {
            _lock: lock,
            previous,
            previous_data,
            previous_override,
            previous_picker_override,
            directory,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
}

impl Drop for XdgConfigHomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var("XDG_CONFIG_HOME", previous),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match self.previous_data.take() {
            Some(previous) => std::env::set_var("XDG_DATA_HOME", previous),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        let mut override_path = test_config_home_cell()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *override_path = self.previous_override.take();
        tui_file_picker::replace_bookmark_config_home_override_for_tests(
            self.previous_picker_override.take(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn guards_serialize_across_test_modules() {
        let (first_ready_tx, first_ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (second_ready_tx, second_ready_rx) = mpsc::channel();

        let first = std::thread::spawn(move || {
            let guard = XdgConfigHomeGuard::new("tonepoet-xdg-first");
            first_ready_tx
                .send(guard.path().to_path_buf())
                .expect("publish first path");
            release_rx.recv().expect("release first guard");
        });
        let first_path = first_ready_rx.recv().expect("first guard acquired");

        let second = std::thread::spawn(move || {
            let guard = XdgConfigHomeGuard::new("tonepoet-xdg-second");
            second_ready_tx
                .send(guard.path().to_path_buf())
                .expect("publish second path");
        });

        assert!(
            second_ready_rx.recv_timeout(Duration::from_millis(75)).is_err(),
            "a second module must not replace XDG_CONFIG_HOME while another test owns it",
        );
        release_tx.send(()).expect("release first");
        let second_path = second_ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second guard acquires after release");
        first.join().expect("first test thread");
        second.join().expect("second test thread");

        assert_ne!(first_path, second_path);
    }

    #[test]
    fn actual_theme_and_picker_resolvers_ignore_live_home_during_external_writes() {
        let live = tempfile::tempdir().expect("live config home");
        let live_root = live.path().to_path_buf();
        let guard = XdgConfigHomeGuard::new("tonepoet-xdg-external-writer");
        let isolated = guard.path().to_path_buf();

        // Recreate the field failure aggressively: while the suite-wide guard
        // owns isolation, make the process environment point back at the live
        // home and concurrently mutate the exact live bookmark pathname. Both
        // real path resolvers must continue using the explicit guarded seam.
        std::env::set_var("XDG_CONFIG_HOME", &live_root);
        let live_bookmarks = live_root.join("tonepoet/bookmarks.toml");
        let writer_path = live_bookmarks.clone();
        let writer = std::thread::spawn(move || {
            std::fs::create_dir_all(writer_path.parent().expect("live parent"))
                .expect("create live writer directory");
            for generation in 0..128 {
                std::fs::write(&writer_path, format!("live-generation={generation}"))
                    .expect("simulate live TUI writer");
                std::thread::yield_now();
            }
        });

        let isolated_bookmarks = isolated.join("tonepoet/bookmarks.toml");
        let records = vec![tui_file_picker::BookmarkRecord {
            name: "Isolated".to_string(),
            path: PathBuf::from("/isolated/bookmark"),
        }];
        tui_file_picker::save_bookmarks_atomic(&records)
            .expect("persist through the picker resolver");
        writer.join().expect("external writer");

        assert_eq!(
            tui_file_picker::bookmark_storage_path(),
            isolated_bookmarks,
            "the picker must not fall back to a cached or subsequently mutated environment path",
        );
        assert!(
            crate::tui::theme::theme_dir().starts_with(&isolated),
            "the theme resolver must share the explicit isolated home",
        );
        assert_eq!(
            tui_file_picker::load_bookmarks().expect("load isolated bookmarks"),
            records,
        );
        assert!(live_bookmarks.exists(), "external writer exercised live path");
        assert_ne!(
            std::fs::read_to_string(&live_bookmarks).expect("read live writer output"),
            std::fs::read_to_string(&isolated_bookmarks).expect("read isolated store"),
        );
    }

}
