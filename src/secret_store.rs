//! OS secret-store seam for archive passwords.
//!
//! Persisted state contains only opaque references. Direct `keyring` crate
//! calls are isolated in `backend`; production builds explicitly select native
//! platform stores, and the seam rejects the non-persistent mock backend unless
//! the test-only in-process store is deliberately enabled.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const REFERENCE_PREFIX: &str = "archive-password:";
const SERVICE: &str = "dev.flox.tonepoet.archive-password";
const TEST_BACKEND_ENV: &str = "TONEPOET_ALLOW_INSECURE_TEST_SECRET_STORE";

static TEST_SECRETS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

#[cfg(test)]
static TEST_BACKEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) struct InsecureTestBackendGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for InsecureTestBackendGuard {
    fn drop(&mut self) {
        std::env::remove_var(TEST_BACKEND_ENV);
        if let Some(secrets) = TEST_SECRETS.get() {
            secrets
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        }
    }
}

#[cfg(test)]
pub(crate) fn insecure_test_secret_count() -> usize {
    TEST_SECRETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

#[cfg(test)]
pub(crate) fn enable_insecure_test_backend() -> InsecureTestBackendGuard {
    let guard = TEST_BACKEND_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::set_var(TEST_BACKEND_ENV, "1");
    TEST_SECRETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    InsecureTestBackendGuard { _guard: guard }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStoreError {
    pub operation: &'static str,
    pub detail: String,
}

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "archive-password secret store {} failed: {}. No cleartext fallback was used",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for SecretStoreError {}

pub fn allocate_reference() -> String {
    format!("{REFERENCE_PREFIX}{}", uuid::Uuid::new_v4())
}

/// Derive an opaque, stable account identifier for a persistence record whose
/// identity is already durable. Repeating an interrupted migration overwrites
/// the same credential instead of accumulating unreferenced random accounts.
pub fn stable_reference(namespace: &str, durable_key: &str) -> Result<String, SecretStoreError> {
    if namespace.is_empty()
        || !namespace
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(error(
            "validate",
            format!("invalid stable-reference namespace '{namespace}'"),
        ));
    }
    if durable_key.is_empty() {
        return Err(error(
            "validate",
            "stable-reference durable key must not be empty",
        ));
    }

    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update((namespace.len() as u64).to_le_bytes());
    digest.update(namespace.as_bytes());
    digest.update((durable_key.len() as u64).to_le_bytes());
    digest.update(durable_key.as_bytes());
    Ok(format!(
        "{REFERENCE_PREFIX}{namespace}-{}",
        hex::encode(digest.finalize())
    ))
}

pub fn store(secret: &str) -> Result<String, SecretStoreError> {
    if secret.is_empty() {
        return Err(error("store", "empty passwords are not storable"));
    }
    let reference = allocate_reference();
    set(&reference, secret)?;
    Ok(reference)
}

pub fn set(reference: &str, secret: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| error("store", "in-process test secret map is poisoned"))?
            .insert(reference.to_string(), secret.to_string());
        return Ok(());
    }
    backend::set(SERVICE, account, secret).map_err(|detail| error("store", detail))
}

pub fn get(reference: &str) -> Result<String, SecretStoreError> {
    let account = account(reference)?;
    if insecure_test_backend_enabled() {
        return TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| error("read", "in-process test secret map is poisoned"))?
            .get(reference)
            .cloned()
            .ok_or_else(|| error("read", format!("reference '{reference}' is unavailable in the opt-in test backend")));
    }
    backend::get(SERVICE, account).map_err(|detail| error("read", detail))
}

pub fn delete(reference: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| error("delete", "in-process test secret map is poisoned"))?
            .remove(reference);
        return Ok(());
    }
    backend::delete(SERVICE, account).map_err(|detail| error("delete", detail))
}

pub fn delete_if_present(reference: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| error("delete", "in-process test secret map is poisoned"))?
            .remove(reference);
        return Ok(());
    }
    backend::delete_if_present(SERVICE, account).map_err(|detail| error("delete", detail))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct PendingSecretPublication {
    /// Newly stored references. Reconciliation revokes any entry that was not
    /// durably published by the authoritative reference file.
    references: Vec<String>,
    /// Superseded store-owned references. Reconciliation revokes each entry
    /// only after the authoritative file no longer names it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retire_after_publish: Vec<String>,
}

pub fn pending_publication_path(publication_path: &Path) -> PathBuf {
    let file_name = publication_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secrets");
    publication_path.with_file_name(format!(".{file_name}.pending-secret-publication.json"))
}

/// Reconcile a crash-left secret-publication journal against the references
/// durably visible in the authoritative file. Published references are kept;
/// unpublished references are revoked before the journal is retired.
pub fn reconcile_pending_publication(
    publication_path: &Path,
    published_references: &[String],
) -> Result<(), String> {
    let journal_path = pending_publication_path(publication_path);
    let bytes = match std::fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "read pending secret-publication journal '{}': {error}",
                journal_path.display()
            ))
        }
    };
    let journal: PendingSecretPublication = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "parse pending secret-publication journal '{}': {error}",
            journal_path.display()
        )
    })?;
    validate_pending_publication(&journal_path, &journal)?;

    let published = published_references.iter().cloned().collect::<HashSet<_>>();
    for reference in &journal.references {
        if !published.contains(reference) {
            delete_if_present(reference).map_err(|error| {
                format!(
                    "revoke unpublished secret reference '{}' from pending journal '{}': {error}",
                    reference,
                    journal_path.display()
                )
            })?;
        }
    }
    for reference in &journal.retire_after_publish {
        if !published.contains(reference) {
            delete_if_present(reference).map_err(|error| {
                format!(
                    "retire superseded secret reference '{}' from pending journal '{}': {error}",
                    reference,
                    journal_path.display()
                )
            })?;
        }
    }
    remove_durable_file(&journal_path).map_err(|error| {
        format!(
            "retire reconciled secret-publication journal '{}': {error}",
            journal_path.display()
        )
    })
}

pub fn begin_pending_publication(
    publication_path: &Path,
    references: &[String],
) -> Result<(), String> {
    begin_pending_publication_with_retirement(publication_path, references, &[])
}

pub fn begin_pending_publication_with_retirement(
    publication_path: &Path,
    references: &[String],
    retire_after_publish: &[String],
) -> Result<(), String> {
    if references.is_empty() && retire_after_publish.is_empty() {
        return Err(
            "pending secret publication requires a new or retiring reference".to_string(),
        );
    }
    let journal = PendingSecretPublication {
        references: references.to_vec(),
        retire_after_publish: retire_after_publish.to_vec(),
    };
    let journal_path = pending_publication_path(publication_path);
    validate_pending_publication(&journal_path, &journal)?;
    if journal_path.exists() {
        return Err(format!(
            "pending secret-publication journal '{}' already exists; reconcile it before starting another publication",
            journal_path.display()
        ));
    }
    let bytes = serde_json::to_vec_pretty(&journal)
        .map_err(|error| format!("serialize pending secret-publication journal: {error}"))?;
    match atomic_write_private_file(&journal_path, &bytes).map_err(|error| {
        format!(
            "publish pending secret-publication journal '{}': {error}",
            journal_path.display()
        )
    })? {
        PrivateFilePublishOutcome::Durable => Ok(()),
        PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(detail) => Err(format!(
            "pending secret-publication journal was replaced but is not durably published: {detail}; no new secret was stored, and the journal must be reconciled before retry"
        )),
    }
}

fn validate_pending_publication(
    journal_path: &Path,
    journal: &PendingSecretPublication,
) -> Result<(), String> {
    if journal.references.is_empty() && journal.retire_after_publish.is_empty() {
        return Err(format!(
            "pending secret-publication journal '{}' contains no new or retiring reference",
            journal_path.display()
        ));
    }
    let mut seen = HashSet::new();
    for reference in &journal.references {
        if !is_reference(reference) {
            return Err(format!(
                "pending secret-publication journal '{}' contains invalid reference '{}'",
                journal_path.display(),
                reference
            ));
        }
        if !seen.insert(reference.as_str()) {
            return Err(format!(
                "pending secret-publication journal '{}' contains duplicate reference '{}'",
                journal_path.display(),
                reference
            ));
        }
    }
    for reference in &journal.retire_after_publish {
        if !is_reference(reference) {
            return Err(format!(
                "pending secret-publication journal '{}' contains invalid retirement reference '{}'",
                journal_path.display(),
                reference
            ));
        }
        if !seen.insert(reference.as_str()) {
            return Err(format!(
                "pending secret-publication journal '{}' names reference '{}' as both new and retiring",
                journal_path.display(),
                reference
            ));
        }
    }
    Ok(())
}

pub fn complete_pending_publication(publication_path: &Path) -> Result<(), String> {
    let journal_path = pending_publication_path(publication_path);
    remove_durable_file(&journal_path).map_err(|error| {
        format!(
            "retire pending secret-publication journal '{}': {error}",
            journal_path.display()
        )
    })
}

pub fn abort_pending_publication(
    publication_path: &Path,
    references: &[String],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for reference in references {
        if let Err(error) = delete_if_present(reference) {
            failures.push(format!("'{reference}': {error}"));
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "failed to revoke unpublished secret reference(s); pending journal retained for retry: {}",
            failures.join(", ")
        ));
    }
    complete_pending_publication(publication_path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrivateFilePublishOutcome {
    Durable,
    ReplacedButDurabilityUnconfirmed(String),
}

pub(crate) fn atomic_write_private_file(
    path: &Path,
    content: &[u8],
) -> std::io::Result<PrivateFilePublishOutcome> {
    atomic_write_private_file_with_sync(
        path,
        content,
        crate::config::sync_publication_parent_dir,
    )
}

fn atomic_write_private_file_with_sync<S>(
    path: &Path,
    content: &[u8],
    mut sync_parent: S,
) -> std::io::Result<PrivateFilePublishOutcome>
where
    S: FnMut(&Path) -> std::io::Result<()>,
{
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("private-file");
    for _ in 0..128 {
        let temporary = parent.join(format!(
            ".{file_name}.tmp.{}",
            uuid::Uuid::new_v4()
        ));
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
        };
        #[cfg(not(unix))]
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let mut published = false;
        let result = (|| {
            file.write_all(content)?;
            file.sync_all()?;
            drop(file);
            crate::config::replace_config_file(&temporary, path)?;
            published = true;
            match sync_parent(parent) {
                Ok(()) => Ok(PrivateFilePublishOutcome::Durable),
                Err(error) => Ok(PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(
                    format!(
                        "'{}' was replaced, but parent-directory durability could not be confirmed: {error}",
                        path.display()
                    ),
                )),
            }
        })();
        if result.is_err() && !published {
            let _ = std::fs::remove_file(&temporary);
        }
        return result;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique private temporary file",
    ))
}

fn remove_durable_file(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => crate::config::sync_publication_parent_dir(
            path.parent().unwrap_or_else(|| Path::new(".")),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn is_reference(value: &str) -> bool {
    value
        .strip_prefix(REFERENCE_PREFIX)
        .is_some_and(|account| !account.is_empty())
}

fn account(reference: &str) -> Result<&str, SecretStoreError> {
    reference
        .strip_prefix(REFERENCE_PREFIX)
        .filter(|account| !account.is_empty())
        .ok_or_else(|| error("validate", format!("invalid secret reference '{reference}'")))
}

fn insecure_test_backend_enabled() -> bool {
    std::env::var(TEST_BACKEND_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

fn error(operation: &'static str, detail: impl Into<String>) -> SecretStoreError {
    SecretStoreError {
        operation,
        detail: detail.into(),
    }
}

/// Direct dependency seam. Applying-side API corrections stay contained here.
mod backend {
    fn entry(service: &str, account: &str) -> Result<keyring::Entry, String> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| format!("keyring backend unavailable: {error}"))?;
        if entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .is_some()
        {
            return Err(
                "keyring selected its non-persistent mock backend; rebuild with a supported platform credential-store feature"
                    .to_string(),
            );
        }
        Ok(entry)
    }

    pub(super) fn set(service: &str, account: &str, secret: &str) -> Result<(), String> {
        entry(service, account)?
            .set_password(secret)
            .map_err(|error| format!("keyring backend rejected secret: {error}"))
    }

    pub(super) fn get(service: &str, account: &str) -> Result<String, String> {
        entry(service, account)?
            .get_password()
            .map_err(|error| format!("keyring reference is unavailable: {error}"))
    }

    pub(super) fn delete(service: &str, account: &str) -> Result<(), String> {
        entry(service, account)?
            .delete_credential()
            .map_err(|error| format!("keyring reference could not be deleted: {error}"))
    }

    pub(super) fn delete_if_present(service: &str, account: &str) -> Result<(), String> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("keyring reference could not be deleted: {error}")),
        }
    }

    #[cfg(test)]
    pub(super) fn default_backend_is_mock() -> Result<bool, String> {
        let entry = keyring::Entry::new(
            "dev.flox.tonepoet.backend-boundary-test",
            "non-persistent-backend-check",
        )
        .map_err(|error| format!("construct keyring boundary-test entry: {error}"))?;
        Ok(entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_validation_rejects_cleartext() {
        assert!(account("hunter2").is_err());
        assert_eq!(account("archive-password:abc").unwrap(), "abc");
    }

    #[test]
    fn stable_references_are_opaque_deterministic_and_domain_separated() {
        let first = stable_reference("queue-item", "durable-item-id").expect("stable reference");
        let repeated =
            stable_reference("queue-item", "durable-item-id").expect("repeated reference");
        let other_namespace =
            stable_reference("other-store", "durable-item-id").expect("other namespace");
        let other_key = stable_reference("queue-item", "other-item-id").expect("other key");

        assert_eq!(first, repeated);
        assert_ne!(first, other_namespace);
        assert_ne!(first, other_key);
        assert!(is_reference(&first));
        assert!(!first.contains("durable-item-id"));
        assert_eq!(
            stable_reference("bad namespace", "id").expect_err("invalid namespace"),
            SecretStoreError {
                operation: "validate",
                detail: "invalid stable-reference namespace 'bad namespace'".to_string(),
            }
        );
        assert_eq!(
            stable_reference("queue-item", "").expect_err("empty key"),
            SecretStoreError {
                operation: "validate",
                detail: "stable-reference durable key must not be empty".to_string(),
            }
        );
    }

    #[test]
    fn opt_in_test_backend_round_trips_without_disk_persistence() {
        let _backend = enable_insecure_test_backend();
        let reference = store("secret-value").expect("store");
        assert!(is_reference(&reference));
        assert_eq!(get(&reference).expect("get"), "secret-value");
        delete(&reference).expect("delete");
        assert!(get(&reference).is_err());
    }
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn production_dependency_selects_a_persistent_platform_backend() {
        assert_eq!(
            backend::default_backend_is_mock().expect("inspect default keyring backend"),
            false
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_reference_file_is_never_classified_as_durable_without_write_through() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pending-secret.json");

        let outcome = atomic_write_private_file(&path, b"journal bytes")
            .expect("replacement itself succeeds");

        assert_eq!(
            outcome,
            PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(format!(
                "'{}' was replaced, but parent-directory durability could not be confirmed: Windows replacement was not performed with write-through semantics",
                path.display()
            ))
        );
        assert_eq!(std::fs::read(&path).expect("published bytes"), b"journal bytes");
    }

    #[test]
    fn private_file_reports_post_rename_parent_sync_failure_without_rollback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("passwords.toml");
        std::fs::write(&path, b"old bytes").expect("write original");

        let outcome = atomic_write_private_file_with_sync(&path, b"new bytes", |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "synthetic parent sync failure",
            ))
        })
        .expect("replacement itself succeeds");

        assert_eq!(
            outcome,
            PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(format!(
                "'{}' was replaced, but parent-directory durability could not be confirmed: synthetic parent sync failure",
                path.display()
            ))
        );
        assert_eq!(std::fs::read(&path).expect("read replaced file"), b"new bytes");
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("list parent")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn pending_publication_reconciliation_retires_superseded_authority_only_after_publish() {
        let _backend = enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = temp.path().join("config.toml");
        let old = stable_reference("config-a", "config-path").expect("old slot");
        let new = stable_reference("config-b", "config-path").expect("new slot");
        set(&old, "old-secret").expect("store old authority");
        set(&new, "new-secret").expect("store new authority");
        begin_pending_publication_with_retirement(
            &publication,
            std::slice::from_ref(&new),
            std::slice::from_ref(&old),
        )
        .expect("journal rotation");

        reconcile_pending_publication(&publication, std::slice::from_ref(&old))
            .expect("unpublished rotation reconciliation");
        assert_eq!(get(&old).expect("old authority retained"), "old-secret");
        assert_eq!(
            get(&new)
                .expect_err("unpublished new authority must be revoked")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                new
            )
        );

        set(&new, "new-secret").expect("restore new authority");
        begin_pending_publication_with_retirement(
            &publication,
            std::slice::from_ref(&new),
            std::slice::from_ref(&old),
        )
        .expect("journal published rotation");
        reconcile_pending_publication(&publication, std::slice::from_ref(&new))
            .expect("published rotation reconciliation");
        assert_eq!(get(&new).expect("new authority retained"), "new-secret");
        assert_eq!(
            get(&old)
                .expect_err("published rotation must retire old authority")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                old
            )
        );
        assert_eq!(insecure_test_secret_count(), 1);
        assert!(!pending_publication_path(&publication).exists());
    }

    #[cfg(unix)]
    #[test]
    fn retirement_only_journal_clears_published_config_authority_after_reference_removal() {
        let _backend = enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = temp.path().join("config.toml");
        let old = stable_reference("config-a", "clear-test").expect("old reference");
        set(&old, "old-secret").expect("store old authority");
        begin_pending_publication_with_retirement(
            &publication,
            &[],
            std::slice::from_ref(&old),
        )
        .expect("journal explicit clear before publication");
        reconcile_pending_publication(&publication, std::slice::from_ref(&old))
            .expect("reconcile failed clear publication");
        assert_eq!(get(&old).expect("old authority retained"), "old-secret");
        assert_eq!(insecure_test_secret_count(), 1);
        assert!(!pending_publication_path(&publication).exists());

        begin_pending_publication_with_retirement(
            &publication,
            &[],
            std::slice::from_ref(&old),
        )
        .expect("journal explicit clear after publication");
        reconcile_pending_publication(&publication, &[])
            .expect("reconcile published reference removal");

        assert_eq!(
            get(&old)
                .expect_err("cleared authority must be retired")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                old
            )
        );
        assert_eq!(insecure_test_secret_count(), 0);
        assert!(!pending_publication_path(&publication).exists());
    }

    #[cfg(unix)]
    #[test]
    fn pending_publication_reconciliation_revokes_only_unpublished_references() {
        let _backend = enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = temp.path().join("config.toml");
        let published = allocate_reference();
        let orphan = allocate_reference();
        set(&published, "published-secret").expect("store published");
        set(&orphan, "orphan-secret").expect("store orphan");
        begin_pending_publication(&publication, &[published.clone(), orphan.clone()])
            .expect("journal pending publication");

        reconcile_pending_publication(&publication, std::slice::from_ref(&published))
            .expect("reconcile publication");

        assert_eq!(get(&published).expect("published secret retained"), "published-secret");
        assert!(get(&orphan).is_err());
        assert!(!pending_publication_path(&publication).exists());
    }

}
