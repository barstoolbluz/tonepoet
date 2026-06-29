#[cfg(test)]
thread_local! {
    static TEST_CONFIG_HOME: std::cell::RefCell<Option<std::path::PathBuf>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn test_config_home_override() -> Option<std::path::PathBuf> {
    TEST_CONFIG_HOME.with(|home| home.borrow().clone())
}

#[cfg(test)]
pub(crate) struct XdgConfigHomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    previous_thread_override: Option<std::path::PathBuf>,
    _tempdir: tempfile::TempDir,
}

#[cfg(test)]
impl XdgConfigHomeGuard {
    pub(crate) fn new(prefix: &str) -> Self {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("XDG_CONFIG_HOME test lock");
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let previous_thread_override = test_config_home_override();
        let tempdir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("temp config home");
        std::env::set_var("XDG_CONFIG_HOME", tempdir.path());
        TEST_CONFIG_HOME.with(|home| {
            *home.borrow_mut() = Some(tempdir.path().to_path_buf());
        });
        Self {
            _lock: lock,
            previous,
            previous_thread_override,
            _tempdir: tempdir,
        }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self._tempdir.path()
    }
}

#[cfg(test)]
impl Drop for XdgConfigHomeGuard {
    fn drop(&mut self) {
        TEST_CONFIG_HOME.with(|home| {
            *home.borrow_mut() = self.previous_thread_override.clone();
        });
        if let Some(previous) = &self.previous {
            std::env::set_var("XDG_CONFIG_HOME", previous);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
