#[cfg(test)]
pub(crate) struct XdgConfigHomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
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
        let tempdir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("temp config home");
        std::env::set_var("XDG_CONFIG_HOME", tempdir.path());
        Self {
            _lock: lock,
            previous,
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
        if let Some(previous) = &self.previous {
            std::env::set_var("XDG_CONFIG_HOME", previous);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
