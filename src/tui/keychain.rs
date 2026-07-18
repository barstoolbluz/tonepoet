//! Archive-password MRU backed by OS secret-store references.
//!
//! `passwords.toml` now stores only opaque reference keys. Legacy cleartext
//! lists are migrated once, with a sibling backup retained for explicit user
//! recovery. There is deliberately no cleartext persistence fallback.

use std::path::{Path, PathBuf};

pub fn keychain_path() -> PathBuf {
    crate::config::TonepoetConfig::config_path().with_file_name("passwords.toml")
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct KeychainFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    references: Vec<String>,
    /// Legacy-only migration input. Never serialized by current code.
    #[serde(default, skip_serializing)]
    passwords: Vec<String>,
}

pub fn load_keychain() -> Result<Vec<String>, String> {
    load_keychain_from_path(&keychain_path())
}

fn load_keychain_from_path(path: &Path) -> Result<Vec<String>, String> {
    let (_lock, target_path) = crate::config::StoreFileLock::acquire_for_path(path)
        .map_err(|error| format!("lock archive-password reference store '{}': {error}", path.display()))?;
    load_keychain_from_locked_path(&target_path)
}

fn load_keychain_from_locked_path(path: &Path) -> Result<Vec<String>, String> {
    let mut file = load_reconciled_file(path)?;
    if !file.passwords.is_empty() {
        file = migrate_legacy_file(path, file)?;
    }

    resolve_references(file.references)
}

fn resolve_references(references: Vec<String>) -> Result<Vec<String>, String> {
    let mut passwords = Vec::with_capacity(references.len());
    for reference in references {
        let password = crate::secret_store::get(&reference).map_err(|error| {
            format!(
                "archive-password reference '{}' could not be resolved: {}",
                reference, error
            )
        })?;
        passwords.push(password);
    }
    Ok(passwords)
}

fn load_file(path: &Path) -> Result<KeychainFile, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(KeychainFile::default()),
        Err(error) => return Err(format!("read archive-password references '{}': {error}", path.display())),
    };
    toml::from_str(&content)
        .map_err(|error| format!("parse archive-password references '{}': {error}", path.display()))
}

fn load_reconciled_file(path: &Path) -> Result<KeychainFile, String> {
    let file = load_file(path)?;
    crate::secret_store::reconcile_pending_publication(path, &file.references)?;
    Ok(file)
}

fn migrate_legacy_file(path: &Path, legacy: KeychainFile) -> Result<KeychainFile, String> {
    let backup = legacy_backup_path(path);
    let mut references = legacy.references;
    let mut known_secrets = Vec::<String>::with_capacity(references.len());
    for reference in &references {
        known_secrets.push(
            crate::secret_store::get(reference)
                .map_err(|error| format!("resolve existing archive-password reference '{reference}' during migration: {error}"))?,
        );
    }

    let mut pending = Vec::<(String, String)>::new();
    for password in legacy.passwords {
        if password.is_empty() || known_secrets.iter().any(|existing| existing == &password) {
            continue;
        }
        let reference = crate::secret_store::allocate_reference();
        known_secrets.push(password.clone());
        references.push(reference.clone());
        pending.push((reference, password));
    }

    let pending_references = pending
        .iter()
        .map(|(reference, _)| reference.clone())
        .collect::<Vec<_>>();
    if !pending_references.is_empty() {
        crate::secret_store::begin_pending_publication(path, &pending_references)?;
        for (reference, password) in &pending {
            if let Err(error) = crate::secret_store::set(reference, password) {
                let primary = format!(
                    "migrate legacy cleartext password list '{}': {error}; original and backup were left unchanged",
                    path.display()
                );
                return Err(with_pending_publication_abort(
                    path,
                    primary,
                    &pending_references,
                ));
            }
        }
    }

    if let Err(error) = create_restricted_legacy_backup(path, &backup) {
        return Err(with_pending_publication_abort(
            path,
            error,
            &pending_references,
        ));
    }

    let migrated = KeychainFile {
        references,
        passwords: Vec::new(),
    };
    let publish_outcome = match save_file(path, &migrated) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(with_pending_publication_abort(
                path,
                error,
                &pending_references,
            ))
        }
    };
    match publish_outcome {
        crate::secret_store::PrivateFilePublishOutcome::Durable => {
            if !pending_references.is_empty() {
                crate::secret_store::reconcile_pending_publication(path, &migrated.references)?;
            }
            Ok(migrated)
        }
        crate::secret_store::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(detail) => {
            let authority = if pending_references.is_empty() {
                "no new secret reference was created"
            } else {
                "the pending secret-publication journal was retained for reconciliation"
            };
            Err(format!(
                "legacy archive-password migration was replaced but is not durably published: {detail}; {authority}"
            ))
        }
    }
}

fn with_pending_publication_abort(
    path: &Path,
    primary: String,
    references: &[String],
) -> String {
    if references.is_empty() {
        return primary;
    }
    match crate::secret_store::abort_pending_publication(path, references) {
        Ok(()) => primary,
        Err(cleanup_error) => format!("{primary}; additionally {cleanup_error}"),
    }
}

#[cfg(test)]
fn cleanup_created_references(references: &[String]) -> Result<(), String> {
    let mut failures = Vec::new();
    for reference in references {
        if let Err(error) = crate::secret_store::delete(reference) {
            failures.push(format!("'{reference}': {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "failed to remove unpublished archive-password reference(s): {}",
            failures.join(", ")
        ))
    }
}

#[cfg(test)]
fn with_reference_cleanup(primary: String, references: &[String]) -> String {
    match cleanup_created_references(references) {
        Ok(()) => primary,
        Err(cleanup_error) => format!("{primary}; additionally {cleanup_error}"),
    }
}

fn create_restricted_legacy_backup(path: &Path, backup: &Path) -> Result<(), String> {
    let source = std::fs::read(path)
        .map_err(|error| format!("read legacy password source '{}': {error}", path.display()))?;
    let created = if backup.exists() {
        let existing = std::fs::read(backup).map_err(|error| {
            format!("read existing legacy password backup '{}': {error}", backup.display())
        })?;
        if source != existing {
            return Err(format!(
                "existing legacy password backup '{}' does not match current source '{}'",
                backup.display(),
                path.display()
            ));
        }
        false
    } else {
        match crate::secret_store::atomic_write_private_file(backup, &source).map_err(|error| {
            format!(
                "back up legacy cleartext password list '{}' to '{}': {error}",
                path.display(),
                backup.display()
            )
        })? {
            crate::secret_store::PrivateFilePublishOutcome::Durable => true,
            crate::secret_store::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(
                detail,
            ) => {
                return Err(format!(
                    "legacy cleartext password backup was replaced but is not durably published: {detail}"
                ))
            }
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(
            backup,
            std::fs::Permissions::from_mode(0o600),
        ) {
            let cleanup_error = created
                .then(|| std::fs::remove_file(backup).err())
                .flatten();
            return Err(legacy_backup_permission_error(
                backup,
                error,
                cleanup_error,
            ));
        }
        std::fs::File::open(backup)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("sync legacy password backup '{}': {error}", backup.display()))?;
    }
    #[cfg(not(unix))]
    let _ = created;
    Ok(())
}

#[cfg(unix)]
fn legacy_backup_permission_error(
    backup: &Path,
    permission_error: std::io::Error,
    cleanup_error: Option<std::io::Error>,
) -> String {
    match cleanup_error {
        Some(cleanup_error) => format!(
            "restrict legacy cleartext password backup '{}': {permission_error}; additionally failed to remove the newly created unrestricted backup: {cleanup_error}",
            backup.display()
        ),
        None => format!(
            "restrict legacy cleartext password backup '{}': {permission_error}",
            backup.display()
        ),
    }
}

fn legacy_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("passwords.toml");
    path.with_file_name(format!("{file_name}.pre-keychain-migration"))
}

fn save_file(
    path: &Path,
    file: &KeychainFile,
) -> Result<crate::secret_store::PrivateFilePublishOutcome, String> {
    let serialized = toml::to_string_pretty(file)
        .map_err(|error| format!("serialize archive-password references: {error}"))?;
    crate::secret_store::atomic_write_private_file(path, serialized.as_bytes())
        .map_err(|error| format!("publish archive-password reference file '{}': {error}", path.display()))
}

fn load_references_locked(path: &Path) -> Result<Vec<String>, String> {
    let mut file = load_reconciled_file(path)?;
    if !file.passwords.is_empty() {
        file = migrate_legacy_file(path, file)?;
    }
    Ok(file.references)
}

fn save_references_locked(path: &Path, references: Vec<String>) -> Result<(), String> {
    match save_file(
        path,
        &KeychainFile {
            references,
            passwords: Vec::new(),
        },
    )? {
        crate::secret_store::PrivateFilePublishOutcome::Durable => Ok(()),
        crate::secret_store::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(detail) => Err(format!(
            "archive-password reference file was replaced but is not durably published: {detail}"
        )),
    }
}

fn lock_reference_store(path: &Path) -> Result<(crate::config::StoreFileLock, PathBuf), String> {
    crate::config::StoreFileLock::acquire_for_path(path)
        .map_err(|error| format!("lock archive-password reference store '{}': {error}", path.display()))
}

pub fn add_password(password: &str) -> Result<(), String> {
    add_password_at_path_with_hook(&keychain_path(), password, |_| {})
}

fn add_password_at_path_with_hook<F>(
    path: &Path,
    password: &str,
    after_secret_stored: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    if password.is_empty() {
        return Err("archive password must not be empty".to_string());
    }
    let (_lock, target_path) = lock_reference_store(path)?;
    add_password_locked(&target_path, password, after_secret_stored)
}

fn add_password_locked<F>(
    path: &Path,
    password: &str,
    after_secret_stored: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let mut references = load_references_locked(path)?;
    let mut existing = None;
    let mut retained = Vec::with_capacity(references.len());
    for reference in references.drain(..) {
        let secret = crate::secret_store::get(&reference).map_err(|error| {
            format!(
                "cannot update archive-password MRU because reference '{}' is unavailable: {}",
                reference, error
            )
        })?;
        if secret == password && existing.is_none() {
            existing = Some(reference);
        } else {
            retained.push(reference);
        }
    }
    match existing {
        Some(reference) => {
            retained.insert(0, reference);
            save_references_locked(path, retained)
        }
        None => {
            let reference = crate::secret_store::allocate_reference();
            retained.insert(0, reference.clone());
            publish_new_reference_locked(
                path,
                &reference,
                password,
                retained,
                after_secret_stored,
            )
        }
    }
}

fn publish_new_reference_locked<F>(
    path: &Path,
    reference: &str,
    password: &str,
    references: Vec<String>,
    mut after_secret_stored: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let pending_references = vec![reference.to_string()];
    crate::secret_store::begin_pending_publication(path, &pending_references)?;
    if let Err(error) = crate::secret_store::set(reference, password) {
        return Err(with_pending_publication_abort(
            path,
            error.to_string(),
            &pending_references,
        ));
    }
    after_secret_stored(reference);
    let publish_outcome = match save_file(
        path,
        &KeychainFile {
            references,
            passwords: Vec::new(),
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(with_pending_publication_abort(
                path,
                error,
                &pending_references,
            ))
        }
    };
    match publish_outcome {
        crate::secret_store::PrivateFilePublishOutcome::Durable => {
            let published = load_file(path)?.references;
            crate::secret_store::reconcile_pending_publication(path, &published)
        }
        crate::secret_store::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(detail) => Err(format!(
            "archive-password reference file was replaced but is not durably published: {detail}; the pending secret-publication journal was retained for reconciliation"
        )),
    }
}

fn remove_reference_from_mru(references: &mut Vec<String>, index: usize) -> Result<String, String> {
    if index >= references.len() {
        return Err(format!("index {index} out of range ({})", references.len()));
    }
    Ok(references.remove(index))
}

pub fn remove_password(index: usize) -> Result<(), String> {
    let path = keychain_path();
    let (_lock, target_path) = lock_reference_store(&path)?;
    let mut references = load_references_locked(&target_path)?;
    let _removed_reference = remove_reference_from_mru(&mut references, index)?;
    save_references_locked(&target_path, references)?;

    // A reference returned by `reference_for_password` can also be persisted by
    // config or a queued conversion. Removing it from the MRU therefore must
    // not revoke the underlying secret. Safe secret garbage collection needs a
    // separate, authoritative reference inventory across every persistent store.
    Ok(())
}

pub fn promote_password(password: &str) -> Result<(), String> {
    add_password(password)
}

/// Store a queue/config password and return its opaque persistence reference.
pub fn reference_for_password(password: &str) -> Result<String, String> {
    let path = keychain_path();
    let (_lock, target_path) = lock_reference_store(&path)?;
    let mut references = load_references_locked(&target_path)?;
    for reference in &references {
        let secret = crate::secret_store::get(reference).map_err(|error| {
            format!(
                "cannot resolve archive-password reference '{}' while deduplicating persisted secrets: {}",
                reference, error
            )
        })?;
        if secret == password {
            return Ok(reference.clone());
        }
    }
    let reference = crate::secret_store::allocate_reference();
    references.insert(0, reference.clone());
    publish_new_reference_locked(
        &target_path,
        &reference,
        password,
        references,
        |_| {},
    )?;
    Ok(reference)
}

pub async fn test_password(archive: &std::path::Path, password: &str) -> Result<bool, String> {
    use tokio::process::Command;

    let bin = crate::detect_7z_binary()
        .ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;
    let output = Command::new(bin)
        .arg("t")
        .arg(archive)
        .arg(format!("-p{password}"))
        .arg("-y")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|error| format!("failed to run {bin}: {error}"))?;
    Ok(output.status.success())
}


#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn reference_file_with_unavailable_secret_is_an_explicit_load_error() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        std::fs::write(
            &path,
            r#"references = ["archive-password:missing-mru-reference"]
"#,
        )
        .expect("write reference file");

        let error = load_keychain_from_path(&path)
            .expect_err("unavailable MRU reference must not become an empty list");

        assert_eq!(
            error,
            "archive-password reference 'archive-password:missing-mru-reference' could not be resolved: archive-password secret store read failed: reference 'archive-password:missing-mru-reference' is unavailable in the opt-in test backend. No cleartext fallback was used"
        );
    }

    #[cfg(unix)]
    #[test]
    fn competing_mru_writer_threads_wait_without_lost_updates_or_secret_revocation() {
        use std::sync::{mpsc, Arc, Barrier};

        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        let stored = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let first_reference = Arc::new(std::sync::Mutex::new(None));

        let first_path = path.clone();
        let first_stored = Arc::clone(&stored);
        let first_release = Arc::clone(&release);
        let captured_first = Arc::clone(&first_reference);
        let first = std::thread::spawn(move || {
            add_password_at_path_with_hook(&first_path, "writer-a", move |reference| {
                *captured_first.lock().expect("capture first reference") =
                    Some(reference.to_string());
                first_stored.wait();
                first_release.wait();
            })
        });

        stored.wait();
        let second_path = path.clone();
        let second_reference = Arc::new(std::sync::Mutex::new(None));
        let captured_second = Arc::clone(&second_reference);
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let result = add_password_at_path_with_hook(&second_path, "writer-b", move |reference| {
                *captured_second.lock().expect("capture second reference") =
                    Some(reference.to_string());
            });
            second_done_tx.send(()).expect("signal second completion");
            result
        });
        assert!(
            second_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "second MRU writer must wait while the first owns the lock"
        );

        release.wait();
        first
            .join()
            .expect("first writer thread")
            .expect("first writer");
        second_done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("second MRU writer completes after release");
        second
            .join()
            .expect("second writer thread")
            .expect("second writer");

        let second_reference = second_reference
            .lock()
            .expect("read second reference")
            .clone()
            .expect("second reference captured");
        let first_reference = first_reference
            .lock()
            .expect("read first reference")
            .clone()
            .expect("first reference captured");
        let file = load_file(&path).expect("read final reference file");
        assert_eq!(
            file.references,
            vec![second_reference.clone(), first_reference.clone()]
        );
        assert_ne!(second_reference, first_reference);
        assert_eq!(
            crate::secret_store::get(&first_reference).expect("first secret survives"),
            "writer-a"
        );
        assert_eq!(
            crate::secret_store::get(&second_reference).expect("second secret survives"),
            "writer-b"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 2);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[test]
    fn unpublished_reference_cleanup_failure_is_attached_to_primary_error() {
        let error = with_reference_cleanup(
            "primary migration failure".to_string(),
            &["not-a-valid-reference".to_string()],
        );

        assert_eq!(
            error,
            "primary migration failure; additionally failed to remove unpublished archive-password reference(s): 'not-a-valid-reference': archive-password secret store validate failed: invalid secret reference 'not-a-valid-reference'. No cleartext fallback was used"
        );
    }

    #[test]
    fn unavailable_reference_is_an_explicit_error_not_an_empty_mru() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let reference = "archive-password:missing-reference".to_string();

        let error = resolve_references(vec![reference.clone()])
            .expect_err("missing reference must fail closed");

        assert_eq!(
            error,
            "archive-password reference 'archive-password:missing-reference' could not be resolved: archive-password secret store read failed: reference 'archive-password:missing-reference' is unavailable in the opt-in test backend. No cleartext fallback was used"
        );
    }

    #[test]
    fn failed_legacy_migration_does_not_create_cleartext_backup_or_rewrite_source() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        let original = b"references = [\"archive-password:missing-reference\"]\npasswords = [\"legacy-cleartext\"]\n";
        std::fs::write(&path, original).expect("write legacy source");

        let error = migrate_legacy_file(
            &path,
            KeychainFile {
                references: vec!["archive-password:missing-reference".to_string()],
                passwords: vec!["legacy-cleartext".to_string()],
            },
        )
        .expect_err("unavailable backend reference must block migration");

        assert_eq!(
            error,
            "resolve existing archive-password reference 'archive-password:missing-reference' during migration: archive-password secret store read failed: reference 'archive-password:missing-reference' is unavailable in the opt-in test backend. No cleartext fallback was used"
        );
        assert_eq!(std::fs::read(&path).expect("read source"), original);
        assert!(!legacy_backup_path(&path).exists());
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
    }

    #[test]
    fn current_file_serialization_contains_references_not_passwords() {
        let file = KeychainFile {
            references: vec!["archive-password:abc".into()],
            passwords: vec!["must-not-serialize".into()],
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(serialized.contains("archive-password:abc"));
        assert!(!serialized.contains("must-not-serialize"));
        assert!(!serialized.contains("passwords ="));
    }

    #[test]
    fn legacy_file_parses_for_migration() {
        let parsed: KeychainFile = toml::from_str("passwords = [\"alpha\", \"bravo\"]")
            .expect("legacy parse");
        assert_eq!(parsed.passwords, vec!["alpha", "bravo"]);
        assert!(parsed.references.is_empty());
    }

    #[test]
    fn removing_an_mru_reference_does_not_revoke_a_shared_persisted_secret() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let reference = crate::secret_store::store("queue-secret").expect("store secret");
        let mut references = vec![reference.clone(), "archive-password:other".to_string()];

        let removed = remove_reference_from_mru(&mut references, 0).expect("remove MRU entry");

        assert_eq!(removed, reference);
        assert_eq!(references, vec!["archive-password:other"]);
        assert_eq!(
            crate::secret_store::get(&removed).expect("shared secret must remain available"),
            "queue-secret"
        );
    }

    #[test]
    fn removing_an_out_of_range_mru_reference_fails_without_mutation() {
        let mut references = vec!["archive-password:one".to_string()];

        let error = remove_reference_from_mru(&mut references, 1)
            .expect_err("out-of-range removal must fail");

        assert_eq!(error, "index 1 out of range (1)");
        assert_eq!(references, vec!["archive-password:one"]);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_backup_permission_error_reports_cleanup_failure() {
        let error = legacy_backup_permission_error(
            Path::new("/tmp/passwords.toml.pre-keychain-migration"),
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "chmod denied"),
            Some(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unlink denied",
            )),
        );

        assert_eq!(
            error,
            "restrict legacy cleartext password backup '/tmp/passwords.toml.pre-keychain-migration': chmod denied; additionally failed to remove the newly created unrestricted backup: unlink denied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_migration_rejects_a_stale_backup_and_removes_new_references() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        let legacy = "passwords = [\"current-secret\"]\n";
        std::fs::write(&path, legacy).expect("legacy password file");
        std::fs::write(legacy_backup_path(&path), "stale backup bytes")
            .expect("stale backup");

        let error = migrate_legacy_file(
            &path,
            load_file(&path).expect("parse legacy password file"),
        )
        .expect_err("stale backup must block migration");

        assert!(
            error.contains("does not match current source"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("source retained"), legacy);
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn matching_existing_legacy_backup_is_restricted_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("passwords.toml");
        let backup = legacy_backup_path(&source);
        let bytes = b"passwords = [\"legacy-secret\"]\n";
        std::fs::write(&source, bytes).expect("source");
        std::fs::write(&backup, bytes).expect("matching backup");
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644))
            .expect("set permissive backup mode");

        create_restricted_legacy_backup(&source, &backup)
            .expect("matching backup should be accepted and restricted");

        assert_eq!(std::fs::read(&backup).expect("backup bytes"), bytes);
        assert_eq!(
            std::fs::metadata(&backup)
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_mru_migration_reconciles_crash_orphan_before_retry() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        std::fs::write(&path, "passwords = [\"legacy-after-crash\"]\n")
            .expect("write legacy password file");
        let orphan = crate::secret_store::allocate_reference();
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&orphan),
        )
        .expect("journal interrupted MRU migration");
        crate::secret_store::set(&orphan, "legacy-after-crash")
            .expect("store simulated orphan");

        let passwords = load_keychain_from_path(&path)
            .expect("reconcile orphan and retry migration");

        assert_eq!(passwords, vec!["legacy-after-crash"]);
        assert!(crate::secret_store::get(&orphan).is_err());
        let migrated = load_file(&path).expect("parse migrated reference file");
        assert_eq!(migrated.references.len(), 1);
        assert_eq!(
            crate::secret_store::get(&migrated.references[0]).expect("published secret"),
            "legacy-after-crash"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_migration_backs_up_cleartext_and_publishes_only_references() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        let legacy = "passwords = [\"alpha-secret\", \"bravo-secret\", \"alpha-secret\"]\n";
        std::fs::write(&path, legacy).expect("legacy password file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("set permissive legacy mode");
        }

        let migrated = migrate_legacy_file(
            &path,
            load_file(&path).expect("parse legacy password file"),
        )
        .expect("migrate legacy password file");

        assert_eq!(migrated.references.len(), 2);
        assert!(migrated.passwords.is_empty());
        assert_eq!(
            crate::secret_store::get(&migrated.references[0]).expect("first secret"),
            "alpha-secret"
        );
        assert_eq!(
            crate::secret_store::get(&migrated.references[1]).expect("second secret"),
            "bravo-secret"
        );
        let rewritten = std::fs::read_to_string(&path).expect("rewritten references");
        assert!(!rewritten.contains("alpha-secret"));
        assert!(!rewritten.contains("bravo-secret"));
        assert!(rewritten.contains("archive-password:"));
        let backup = legacy_backup_path(&path);
        assert_eq!(
            std::fs::read_to_string(&backup).expect("legacy backup"),
            legacy
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup)
                    .expect("legacy backup metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
