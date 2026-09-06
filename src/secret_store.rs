//! Platform secret-store seam for archive passwords.
//!
//! Persisted state contains only opaque references. Direct `keyring` crate
//! calls are isolated in `backend`. macOS and Windows use the native credential
//! store. Linux defaults to a non-interactive authenticated-encryption file
//! under the tonepoet config directory, protected by a machine-local 256-bit
//! key stored beside it with owner-only permissions.
//!
//! Threat model for the Linux backend: it prevents passwords from appearing as
//! cleartext in configuration, queue, history, ordinary backups, and casual
//! filesystem inspection. It does not protect against a process already able
//! to read both files as the same OS user. That tradeoff is deliberate: archive
//! passwords must remain usable on headless systems without an unlock prompt.

use std::collections::HashSet;
#[cfg(any(test, debug_assertions))]
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::{Mutex, OnceLock};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

const REFERENCE_PREFIX: &str = "archive-password:";
const SERVICE: &str = "dev.flox.tonepoet.archive-password";
#[cfg(any(test, debug_assertions))]
const TEST_BACKEND_ENV: &str = "TONEPOET_ALLOW_INSECURE_TEST_SECRET_STORE";

#[cfg(any(test, debug_assertions))]
static TEST_SECRETS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

#[cfg(test)]
static TEST_BACKEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
#[cfg(test)]
static TEST_BACKEND_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) struct InsecureTestBackendGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for InsecureTestBackendGuard {
    fn drop(&mut self) {
        TEST_BACKEND_UNAVAILABLE.store(false, Ordering::SeqCst);
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
    TEST_BACKEND_UNAVAILABLE.store(false, Ordering::SeqCst);
    TEST_SECRETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    InsecureTestBackendGuard { _guard: guard }
}

#[cfg(test)]
pub(crate) fn enable_unavailable_test_backend() -> InsecureTestBackendGuard {
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
    TEST_BACKEND_UNAVAILABLE.store(true, Ordering::SeqCst);
    InsecureTestBackendGuard { _guard: guard }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreErrorKind {
    Unavailable,
    NotFound,
    Corrupt,
    PermissionDenied,
    InvalidInput,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStoreError {
    pub kind: SecretStoreErrorKind,
    pub operation: &'static str,
    pub detail: String,
}

impl SecretStoreError {
    pub fn is_backend_unavailable(&self) -> bool {
        self.kind == SecretStoreErrorKind::Unavailable
    }

    pub fn is_not_found(&self) -> bool {
        self.kind == SecretStoreErrorKind::NotFound
    }
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
            SecretStoreErrorKind::InvalidInput,
            "validate",
            format!("invalid stable-reference namespace '{namespace}'"),
        ));
    }
    if durable_key.is_empty() {
        return Err(error(
            SecretStoreErrorKind::InvalidInput,
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
        return Err(error(
            SecretStoreErrorKind::InvalidInput,
            "store",
            "empty passwords are not storable",
        ));
    }
    let reference = allocate_reference();
    set(&reference, secret)?;
    Ok(reference)
}

pub fn set(reference: &str, secret: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    #[cfg(test)]
    if TEST_BACKEND_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(error(
            SecretStoreErrorKind::Unavailable,
            "store",
            "injected unavailable secret backend",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| {
                error(
                    SecretStoreErrorKind::Other,
                    "store",
                    "in-process test secret map is poisoned",
                )
            })?
            .insert(reference.to_string(), secret.to_string());
        return Ok(());
    }
    backend::set(SERVICE, account, secret).map_err(|error| error.with_operation("store"))
}

pub fn get(reference: &str) -> Result<String, SecretStoreError> {
    let account = account(reference)?;
    #[cfg(test)]
    if TEST_BACKEND_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(error(
            SecretStoreErrorKind::Unavailable,
            "read",
            "injected unavailable secret backend",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if insecure_test_backend_enabled() {
        return TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| {
                error(
                    SecretStoreErrorKind::Other,
                    "read",
                    "in-process test secret map is poisoned",
                )
            })?
            .get(reference)
            .cloned()
            .ok_or_else(|| {
                error(
                    SecretStoreErrorKind::NotFound,
                    "read",
                    format!(
                        "reference '{reference}' is unavailable in the opt-in test backend"
                    ),
                )
            });
    }
    backend::get(SERVICE, account).map_err(|error| error.with_operation("read"))
}

pub fn delete(reference: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    #[cfg(test)]
    if TEST_BACKEND_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(error(
            SecretStoreErrorKind::Unavailable,
            "delete",
            "injected unavailable secret backend",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| {
                error(
                    SecretStoreErrorKind::Other,
                    "delete",
                    "in-process test secret map is poisoned",
                )
            })?
            .remove(reference);
        return Ok(());
    }
    backend::delete(SERVICE, account).map_err(|error| error.with_operation("delete"))
}

pub fn delete_if_present(reference: &str) -> Result<(), SecretStoreError> {
    let account = account(reference)?;
    #[cfg(test)]
    if TEST_BACKEND_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(error(
            SecretStoreErrorKind::Unavailable,
            "delete",
            "injected unavailable secret backend",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if insecure_test_backend_enabled() {
        TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| {
                error(
                    SecretStoreErrorKind::Other,
                    "delete",
                    "in-process test secret map is poisoned",
                )
            })?
            .remove(reference);
        return Ok(());
    }
    backend::delete_if_present(SERVICE, account)
        .map_err(|error| error.with_operation("delete"))
}

/// Delete a set of references while amortizing encrypted-file read/decrypt and
/// durable publication to one transaction on Linux. All references are
/// validated before any backend mutation begins.
pub fn delete_many_if_present(references: &[String]) -> Result<(), SecretStoreError> {
    let accounts = references
        .iter()
        .map(|reference| account(reference))
        .collect::<Result<Vec<_>, _>>()?;
    if accounts.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    if TEST_BACKEND_UNAVAILABLE.load(Ordering::SeqCst) {
        return Err(error(
            SecretStoreErrorKind::Unavailable,
            "delete",
            "injected unavailable secret backend",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if insecure_test_backend_enabled() {
        let mut secrets = TEST_SECRETS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| {
                error(
                    SecretStoreErrorKind::Other,
                    "delete",
                    "in-process test secret map is poisoned",
                )
            })?;
        for reference in references {
            secrets.remove(reference);
        }
        return Ok(());
    }
    backend::delete_many_if_present(SERVICE, &accounts)
        .map_err(|error| error.with_operation("delete"))
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
    reconcile_pending_publication_classified(publication_path, published_references)
        .map_err(|error| error.to_string())
}

#[derive(Debug)]
pub enum PendingPublicationReconcileError {
    Journal(String),
    Secret {
        context: String,
        source: SecretStoreError,
    },
    Durability(String),
}

impl PendingPublicationReconcileError {
    pub fn is_backend_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Secret { source, .. } if source.is_backend_unavailable()
        )
    }
}

impl std::fmt::Display for PendingPublicationReconcileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal(message) | Self::Durability(message) => formatter.write_str(message),
            Self::Secret { context, source } => write!(formatter, "{context}: {source}"),
        }
    }
}

impl std::error::Error for PendingPublicationReconcileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Secret { source, .. } => Some(source),
            Self::Journal(_) | Self::Durability(_) => None,
        }
    }
}

pub fn reconcile_pending_publication_classified(
    publication_path: &Path,
    published_references: &[String],
) -> Result<(), PendingPublicationReconcileError> {
    let journal_path = pending_publication_path(publication_path);
    let bytes = match std::fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(PendingPublicationReconcileError::Journal(format!(
                "read pending secret-publication journal '{}': {error}",
                journal_path.display()
            )))
        }
    };
    let journal: PendingSecretPublication = serde_json::from_slice(&bytes).map_err(|error| {
        PendingPublicationReconcileError::Journal(format!(
            "parse pending secret-publication journal '{}': {error}",
            journal_path.display()
        ))
    })?;
    validate_pending_publication(&journal_path, &journal)
        .map_err(PendingPublicationReconcileError::Journal)?;

    let published = published_references.iter().cloned().collect::<HashSet<_>>();
    let mut revoke = journal
        .references
        .iter()
        .chain(&journal.retire_after_publish)
        .filter(|reference| !published.contains(*reference))
        .cloned()
        .collect::<Vec<_>>();
    revoke.sort();
    revoke.dedup();
    delete_many_if_present(&revoke).map_err(|source| {
        PendingPublicationReconcileError::Secret {
            context: format!(
                "reconcile {} unpublished or superseded secret reference(s) from pending journal '{}'",
                revoke.len(),
                journal_path.display()
            ),
            source,
        }
    })?;
    remove_durable_file(&journal_path).map_err(|error| {
        PendingPublicationReconcileError::Durability(format!(
            "retire reconciled secret-publication journal '{}': {error}",
            journal_path.display()
        ))
    })
}

/// Retire a pending publication journal without contacting the secret backend.
///
/// This is an explicit headless recovery escape hatch. The journal is parsed
/// and validated before removal, but unpublished references are not revoked;
/// callers must surface that orphan risk to the user.
pub fn retire_pending_publication_journal_headless(
    publication_path: &Path,
) -> Result<usize, String> {
    let (_lock, publication_path) = crate::config::StoreFileLock::acquire_for_path(publication_path)
        .map_err(|error| {
            format!(
                "lock publication target '{}' before retiring its pending secret journal: {error}",
                publication_path.display()
            )
        })?;
    let journal_path = pending_publication_path(&publication_path);
    let bytes = match std::fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
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
    let at_risk = journal
        .references
        .iter()
        .chain(journal.retire_after_publish.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    remove_durable_file(&journal_path).map_err(|error| {
        format!(
            "retire pending secret-publication journal '{}': {error}",
            journal_path.display()
        )
    })?;
    Ok(at_risk)
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
    delete_many_if_present(references).map_err(|error| {
        format!(
            "failed to revoke {} unpublished secret reference(s); pending journal retained for retry: {error}",
            references.len()
        )
    })?;
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

/// Return true only for a stable reference minted by the named ownership
/// namespace. This is used before garbage collection so shared MRU/config
/// authorities are never revoked as if they belonged to a queue row.
pub fn reference_has_namespace(value: &str, namespace: &str) -> bool {
    let prefix = format!("{namespace}-");
    value
        .strip_prefix(REFERENCE_PREFIX)
        .and_then(|account| account.strip_prefix(&prefix))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn account(reference: &str) -> Result<&str, SecretStoreError> {
    reference
        .strip_prefix(REFERENCE_PREFIX)
        .filter(|account| !account.is_empty())
        .ok_or_else(|| {
            error(
                SecretStoreErrorKind::InvalidInput,
                "validate",
                format!("invalid secret reference '{reference}'"),
            )
        })
}

#[cfg(any(test, debug_assertions))]
fn insecure_test_backend_enabled() -> bool {
    std::env::var(TEST_BACKEND_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

fn error(
    kind: SecretStoreErrorKind,
    operation: &'static str,
    detail: impl Into<String>,
) -> SecretStoreError {
    SecretStoreError {
        kind,
        operation,
        detail: detail.into(),
    }
}

#[derive(Debug)]
struct BackendError {
    kind: SecretStoreErrorKind,
    detail: String,
}

impl BackendError {
    fn new(kind: SecretStoreErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    fn with_operation(self, operation: &'static str) -> SecretStoreError {
        error(self.kind, operation, self.detail)
    }
}

/// Direct dependency seam. Applying-side API corrections stay contained here.
mod backend {
    use super::{BackendError, SecretStoreErrorKind};

    #[cfg(target_os = "linux")]
    pub(super) fn set(_service: &str, account: &str, secret: &str) -> Result<(), BackendError> {
        encrypted_file::set(account, secret)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn set(service: &str, account: &str, secret: &str) -> Result<(), BackendError> {
        native_keyring::set(service, account, secret)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn get(service: &str, account: &str) -> Result<String, BackendError> {
        match encrypted_file::get(account) {
            Ok(secret) => Ok(secret),
            Err(error) if error.kind == SecretStoreErrorKind::NotFound => {
                match native_keyring::get(service, account) {
                    Ok(secret) => {
                        encrypted_file::set(account, &secret)?;
                        if let Err(delete_error) =
                            native_keyring::delete_if_present(service, account)
                        {
                            log::warn!(
                                "archive-password reference '{}' migrated to the Linux encrypted-file backend, but its legacy keyring entry could not be retired: {}",
                                account,
                                delete_error.detail
                            );
                        }
                        Ok(secret)
                    }
                    Err(legacy_error) if legacy_error.kind == SecretStoreErrorKind::NotFound => {
                        Err(error)
                    }
                    Err(legacy_error) => Err(BackendError::new(
                        legacy_error.kind,
                        format!(
                            "encrypted-file reference is absent and legacy keyring migration could not be attempted: {}",
                            legacy_error.detail
                        ),
                    )),
                }
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn get(service: &str, account: &str) -> Result<String, BackendError> {
        native_keyring::get(service, account)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn delete(service: &str, account: &str) -> Result<(), BackendError> {
        let missing = encrypted_file::delete_many_if_present(&[account])?;
        if missing.is_empty() {
            return Ok(());
        }
        native_keyring::delete(service, account)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn delete(service: &str, account: &str) -> Result<(), BackendError> {
        native_keyring::delete(service, account)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn delete_if_present(service: &str, account: &str) -> Result<(), BackendError> {
        delete_many_if_present(service, &[account])
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn delete_if_present(service: &str, account: &str) -> Result<(), BackendError> {
        native_keyring::delete_if_present(service, account)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn delete_many_if_present(
        service: &str,
        accounts: &[&str],
    ) -> Result<(), BackendError> {
        for account in encrypted_file::delete_many_if_present(accounts)? {
            native_keyring::delete_if_present(service, &account)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn delete_many_if_present(
        service: &str,
        accounts: &[&str],
    ) -> Result<(), BackendError> {
        for account in accounts {
            native_keyring::delete_if_present(service, account)?;
        }
        Ok(())
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn encrypted_file_round_trip_at_path(path: &std::path::Path) -> Result<(), String> {
        encrypted_file::round_trip_at_path(path).map_err(|error| error.detail)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn encrypted_file_get_at_path(
        path: &std::path::Path,
        account: &str,
    ) -> Result<String, (SecretStoreErrorKind, String)> {
        encrypted_file::get_at_path(path, account)
            .map_err(|error| (error.kind, error.detail))
    }

    #[cfg(test)]
    pub(super) fn default_backend_is_mock() -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        {
            Ok(false)
        }
        #[cfg(not(target_os = "linux"))]
        {
            native_keyring::default_backend_is_mock()
        }
    }

    mod native_keyring {
        use super::{BackendError, SecretStoreErrorKind};

        fn entry(service: &str, account: &str) -> Result<keyring::Entry, BackendError> {
            let entry = keyring::Entry::new(service, account).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Unavailable,
                    format!("keyring backend unavailable: {error}"),
                )
            })?;
            if entry
                .get_credential()
                .downcast_ref::<keyring::mock::MockCredential>()
                .is_some()
            {
                return Err(BackendError::new(
                    SecretStoreErrorKind::Unavailable,
                    "keyring selected its non-persistent mock backend; rebuild with a supported platform credential-store feature",
                ));
            }
            Ok(entry)
        }

        // Unused on Linux by design: writes go to the encrypted-file store,
        // while `get`/`delete_if_present` stay live for one-shot migration of
        // legacy keyring entries. macOS/Windows route writes here.
        #[cfg_attr(target_os = "linux", allow(dead_code))]
        pub(super) fn set(
            service: &str,
            account: &str,
            secret: &str,
        ) -> Result<(), BackendError> {
            entry(service, account)?
                .set_password(secret)
                .map_err(|error| classify_keyring_error("keyring backend rejected secret", error))
        }

        pub(super) fn get(service: &str, account: &str) -> Result<String, BackendError> {
            entry(service, account)?
                .get_password()
                .map_err(|error| classify_keyring_error("keyring reference is unavailable", error))
        }

        pub(super) fn delete(service: &str, account: &str) -> Result<(), BackendError> {
            entry(service, account)?
                .delete_credential()
                .map_err(|error| classify_keyring_error("keyring reference could not be deleted", error))
        }

        pub(super) fn delete_if_present(
            service: &str,
            account: &str,
        ) -> Result<(), BackendError> {
            match entry(service, account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(classify_keyring_error(
                    "keyring reference could not be deleted",
                    error,
                )),
            }
        }

        fn classify_keyring_error(context: &str, error: keyring::Error) -> BackendError {
            let kind = match error {
                keyring::Error::NoEntry => SecretStoreErrorKind::NotFound,
                keyring::Error::Ambiguous(_) | keyring::Error::BadEncoding(_) => {
                    SecretStoreErrorKind::Corrupt
                }
                keyring::Error::PlatformFailure(_) => SecretStoreErrorKind::Unavailable,
                _ => SecretStoreErrorKind::Other,
            };
            BackendError::new(kind, format!("{context}: {error}"))
        }

        #[cfg(all(test, not(target_os = "linux")))]
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

    // NEEDS-VERIFICATION (apply side): this thin module is the only direct
    // `ring` API boundary; the 0.17 signatures could not be compiler-checked
    // in the offline handoff environment.
    #[cfg(target_os = "linux")]
    mod encrypted_file {
        use super::{BackendError, SecretStoreErrorKind};
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
        use std::collections::BTreeMap;
        use std::io::Read;
        use std::path::{Path, PathBuf};

        const MAGIC: &[u8; 8] = b"TPSECR01";
        const VERSION: u8 = 1;
        const NONCE_LEN: usize = 12;
        const KEY_LEN: usize = 32;

        fn store_path() -> Result<PathBuf, BackendError> {
            let base = dirs::config_dir().ok_or_else(|| {
                BackendError::new(
                    SecretStoreErrorKind::Unavailable,
                    "cannot determine the user configuration directory for the Linux encrypted-file secret store",
                )
            })?;
            Ok(base.join("tonepoet").join("archive-secrets.enc"))
        }

        fn key_path(store_path: &Path) -> PathBuf {
            store_path.with_file_name("archive-secrets.key")
        }

        pub(super) fn set(account: &str, secret: &str) -> Result<(), BackendError> {
            with_locked_store(|store_path| {
                let store_exists = secure_regular_file_exists(store_path)?;
                let key_path = key_path(store_path);
                let key = if store_exists {
                    read_key_for_existing_store(&key_path, store_path)?
                } else {
                    read_or_create_key(&key_path)?
                };
                let mut secrets = if store_exists {
                    read_secrets(store_path, &key)?
                } else {
                    BTreeMap::new()
                };
                secrets.insert(account.to_string(), secret.to_string());
                write_secrets(store_path, &key, &secrets)
            })
        }

        pub(super) fn get(account: &str) -> Result<String, BackendError> {
            with_locked_store(|store_path| {
                if !secure_regular_file_exists(store_path)? {
                    return Err(BackendError::new(
                        SecretStoreErrorKind::NotFound,
                        format!("reference '{account}' is absent from the Linux encrypted-file store"),
                    ));
                }
                let key = read_key_for_existing_store(&key_path(store_path), store_path)?;
                let secrets = read_secrets(store_path, &key)?;
                secrets.get(account).cloned().ok_or_else(|| {
                    BackendError::new(
                        SecretStoreErrorKind::NotFound,
                        format!("reference '{account}' is absent from the Linux encrypted-file store"),
                    )
                })
            })
        }

        pub(super) fn delete_many_if_present(
            accounts: &[&str],
        ) -> Result<Vec<String>, BackendError> {
            with_locked_store(|store_path| {
                if !secure_regular_file_exists(store_path)? {
                    return Ok(accounts.iter().map(|account| (*account).to_string()).collect());
                }
                let key = read_key_for_existing_store(&key_path(store_path), store_path)?;
                let mut secrets = read_secrets(store_path, &key)?;
                let mut missing = Vec::new();
                let mut changed = false;
                for account in accounts {
                    if secrets.remove(*account).is_some() {
                        changed = true;
                    } else {
                        missing.push((*account).to_string());
                    }
                }
                if changed {
                    write_secrets(store_path, &key, &secrets)?;
                }
                Ok(missing)
            })
        }

        fn with_locked_store<T>(
            operation: impl FnOnce(&Path) -> Result<T, BackendError>,
        ) -> Result<T, BackendError> {
            let path = store_path()?;
            let (_lock, target_path) = crate::config::StoreFileLock::acquire_for_path(&path)
                .map_err(|error| {
                    BackendError::new(
                        SecretStoreErrorKind::Unavailable,
                        format!(
                            "lock Linux encrypted-file secret store '{}': {error}",
                            path.display()
                        ),
                    )
                })?;
            operation(&target_path)
        }

        fn secure_regular_file_exists(path: &Path) -> Result<bool, BackendError> {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    Err(BackendError::new(
                        SecretStoreErrorKind::PermissionDenied,
                        format!("private path '{}' is not a regular file", path.display()),
                    ))
                }
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(classify_io_error(
                    format!("inspect private file '{}'", path.display()),
                    error,
                )),
            }
        }

        fn read_key(path: &Path) -> Result<[u8; KEY_LEN], BackendError> {
            let mut file = open_private_read(path)?;
            let mut key = [0u8; KEY_LEN];
            file.read_exact(&mut key).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    format!("read Linux secret-store key '{}': {error}", path.display()),
                )
            })?;
            let mut trailing = [0u8; 1];
            if file.read(&mut trailing).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    format!("validate Linux secret-store key '{}': {error}", path.display()),
                )
            })? != 0
            {
                return Err(BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    format!(
                        "Linux secret-store key '{}' is not exactly {KEY_LEN} bytes",
                        path.display()
                    ),
                ));
            }
            restrict_private_permissions(path)?;
            Ok(key)
        }

        fn read_key_for_existing_store(
            path: &Path,
            store_path: &Path,
        ) -> Result<[u8; KEY_LEN], BackendError> {
            match read_key(path) {
                Ok(key) => Ok(key),
                Err(error) if error.kind == SecretStoreErrorKind::NotFound => {
                    Err(BackendError::new(
                        SecretStoreErrorKind::Corrupt,
                        format!(
                            "Linux encrypted secret store '{}' exists, but its key '{}' is missing",
                            store_path.display(),
                            path.display()
                        ),
                    ))
                }
                Err(error) => Err(error),
            }
        }

        fn read_or_create_key(path: &Path) -> Result<[u8; KEY_LEN], BackendError> {
            match read_key(path) {
                Ok(key) => Ok(key),
                Err(error) if error.kind == SecretStoreErrorKind::NotFound => create_key(path),
                Err(error) => Err(error),
            }
        }

        fn create_key(path: &Path) -> Result<[u8; KEY_LEN], BackendError> {
            let mut key = [0u8; KEY_LEN];
            getrandom::getrandom(&mut key).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Unavailable,
                    format!("generate Linux secret-store key: {error}"),
                )
            })?;
            match super::super::atomic_write_private_file(path, &key).map_err(|error| {
                classify_io_error(
                    format!("publish Linux secret-store key '{}'", path.display()),
                    error,
                )
            })? {
                super::super::PrivateFilePublishOutcome::Durable => Ok(key),
                super::super::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(
                    detail,
                ) => Err(BackendError::new(SecretStoreErrorKind::Unavailable, detail)),
            }
        }

        fn read_secrets(
            path: &Path,
            key: &[u8; KEY_LEN],
        ) -> Result<BTreeMap<String, String>, BackendError> {
            let mut file = match open_private_read(path) {
                Ok(file) => file,
                Err(error) if error.kind == SecretStoreErrorKind::NotFound => {
                    return Ok(BTreeMap::new())
                }
                Err(error) => return Err(error),
            };
            restrict_private_permissions(path)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|error| {
                classify_io_error(
                    format!("read Linux encrypted secret store '{}'", path.display()),
                    error,
                )
            })?;
            if bytes.len() < MAGIC.len() + 1 + NONCE_LEN + 16
                || &bytes[..MAGIC.len()] != MAGIC
                || bytes[MAGIC.len()] != VERSION
            {
                return Err(BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    format!(
                        "Linux encrypted secret store '{}' has an invalid header",
                        path.display()
                    ),
                ));
            }
            let nonce_start = MAGIC.len() + 1;
            let ciphertext_start = nonce_start + NONCE_LEN;
            let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| {
                BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    "Linux secret-store key has an invalid length",
                )
            })?);
            let nonce_bytes: [u8; NONCE_LEN] = bytes[nonce_start..ciphertext_start]
                .try_into()
                .expect("validated Linux secret-store nonce slice");
            let mut ciphertext = bytes[ciphertext_start..].to_vec();
            let plaintext = key
                .open_in_place(
                    Nonce::assume_unique_for_key(nonce_bytes),
                    Aad::empty(),
                    &mut ciphertext,
                )
                .map_err(|_| {
                    BackendError::new(
                        SecretStoreErrorKind::Corrupt,
                        format!(
                            "Linux encrypted secret store '{}' failed authentication",
                            path.display()
                        ),
                    )
                })?;
            serde_json::from_slice(plaintext).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    format!(
                        "parse Linux encrypted secret store '{}': {error}",
                        path.display()
                    ),
                )
            })
        }

        fn write_secrets(
            path: &Path,
            key: &[u8; KEY_LEN],
            secrets: &BTreeMap<String, String>,
        ) -> Result<(), BackendError> {
            let plaintext = serde_json::to_vec(secrets).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Other,
                    format!("serialize Linux encrypted secret store: {error}"),
                )
            })?;
            let mut nonce = [0u8; NONCE_LEN];
            getrandom::getrandom(&mut nonce).map_err(|error| {
                BackendError::new(
                    SecretStoreErrorKind::Unavailable,
                    format!("generate Linux secret-store nonce: {error}"),
                )
            })?;
            let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| {
                BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    "Linux secret-store key has an invalid length",
                )
            })?);
            let mut ciphertext = plaintext;
            key.seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut ciphertext,
            )
            .map_err(|_| {
                BackendError::new(
                    SecretStoreErrorKind::Other,
                    "encrypt Linux secret-store payload",
                )
            })?;
            let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + NONCE_LEN + ciphertext.len());
            bytes.extend_from_slice(MAGIC);
            bytes.push(VERSION);
            bytes.extend_from_slice(&nonce);
            bytes.extend_from_slice(&ciphertext);
            match super::super::atomic_write_private_file(path, &bytes).map_err(|error| {
                classify_io_error(
                    format!("publish Linux encrypted secret store '{}'", path.display()),
                    error,
                )
            })? {
                super::super::PrivateFilePublishOutcome::Durable => Ok(()),
                super::super::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(
                    detail,
                ) => Err(BackendError::new(SecretStoreErrorKind::Unavailable, detail)),
            }
        }

        fn open_private_read(path: &Path) -> Result<std::fs::File, BackendError> {
            #[cfg(unix)]
            let opened = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
            };
            #[cfg(not(unix))]
            let opened = std::fs::OpenOptions::new().read(true).open(path);
            opened.map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    BackendError::new(
                        SecretStoreErrorKind::NotFound,
                        format!("private file '{}' does not exist", path.display()),
                    )
                } else {
                    classify_io_error(format!("open private file '{}'", path.display()), error)
                }
            })
        }

        fn restrict_private_permissions(path: &Path) -> Result<(), BackendError> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                    classify_io_error(format!("inspect private file '{}'", path.display()), error)
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(BackendError::new(
                        SecretStoreErrorKind::PermissionDenied,
                        format!("private path '{}' is not a regular file", path.display()),
                    ));
                }
                if metadata.permissions().mode() & 0o077 != 0 {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                        .map_err(|error| {
                            classify_io_error(
                                format!("restrict private file '{}'", path.display()),
                                error,
                            )
                        })?;
                }
            }
            Ok(())
        }

        fn classify_io_error(context: String, error: std::io::Error) -> BackendError {
            let kind = match error.kind() {
                std::io::ErrorKind::NotFound => SecretStoreErrorKind::NotFound,
                std::io::ErrorKind::PermissionDenied => SecretStoreErrorKind::PermissionDenied,
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    SecretStoreErrorKind::Unavailable
                }
                std::io::ErrorKind::InvalidData => SecretStoreErrorKind::Corrupt,
                std::io::ErrorKind::InvalidInput => SecretStoreErrorKind::InvalidInput,
                _ => SecretStoreErrorKind::Other,
            };
            BackendError::new(kind, format!("{context}: {error}"))
        }

        #[cfg(test)]
        pub(super) fn round_trip_at_path(path: &Path) -> Result<(), BackendError> {
            let key = read_or_create_key(&key_path(path))?;
            let mut values = BTreeMap::new();
            values.insert("test-account".to_string(), "test-secret".to_string());
            write_secrets(path, &key, &values)?;
            let read = read_secrets(path, &key)?;
            if read != values {
                return Err(BackendError::new(
                    SecretStoreErrorKind::Corrupt,
                    "encrypted-file round-trip changed values",
                ));
            }
            Ok(())
        }

        #[cfg(test)]
        pub(super) fn get_at_path(path: &Path, account: &str) -> Result<String, BackendError> {
            if !secure_regular_file_exists(path)? {
                return Err(BackendError::new(
                    SecretStoreErrorKind::NotFound,
                    format!("reference '{account}' is absent from the Linux encrypted-file store"),
                ));
            }
            let key = read_key_for_existing_store(&key_path(path), path)?;
            let secrets = read_secrets(path, &key)?;
            secrets.get(account).cloned().ok_or_else(|| {
                BackendError::new(
                    SecretStoreErrorKind::NotFound,
                    format!("reference '{account}' is absent from the Linux encrypted-file store"),
                )
            })
        }
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
                kind: SecretStoreErrorKind::InvalidInput,
                operation: "validate",
                detail: "invalid stable-reference namespace 'bad namespace'".to_string(),
            }
        );
        assert_eq!(
            stable_reference("queue-item", "").expect_err("empty key"),
            SecretStoreError {
                kind: SecretStoreErrorKind::InvalidInput,
                operation: "validate",
                detail: "stable-reference durable key must not be empty".to_string(),
            }
        );
    }

    #[test]
    fn stable_reference_namespace_check_does_not_claim_shared_references() {
        let queue = stable_reference("queue-item", "item-1").expect("queue reference");
        let config = stable_reference("config-a", "config").expect("config reference");
        assert!(reference_has_namespace(&queue, "queue-item"));
        assert!(!reference_has_namespace(&config, "queue-item"));
        assert!(!reference_has_namespace("archive-password:queue-item", "queue-item"));
        assert!(!reference_has_namespace(
            "archive-password:queue-item-evil-0123456789abcdef0123456789abcdef0123456789abcdef",
            "queue-item"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_encrypted_file_backend_round_trips_authenticated_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("archive-secrets.enc");
        backend::encrypted_file_round_trip_at_path(&path).expect("encrypted round trip");
        assert_ne!(
            std::fs::read(&path).expect("encrypted bytes"),
            br#"{"test-account":"test-secret"}"#
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o077,
                0
            );
            assert_eq!(
                std::fs::metadata(temp.path().join("archive-secrets.key"))
                    .expect("key metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_encrypted_file_store_never_rekeys_an_existing_store_when_key_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("archive-secrets.enc");
        backend::encrypted_file_round_trip_at_path(&path).expect("encrypted round trip");
        let key_path = temp.path().join("archive-secrets.key");
        std::fs::remove_file(&key_path).expect("remove key");

        let (kind, detail) = backend::encrypted_file_get_at_path(&path, "test-account")
            .expect_err("an existing ciphertext without its key must fail closed");

        assert_eq!(kind, SecretStoreErrorKind::Corrupt);
        assert_eq!(
            detail,
            format!(
                "Linux encrypted secret store '{}' exists, but its key '{}' is missing",
                path.display(),
                key_path.display()
            )
        );
        assert!(!key_path.exists(), "read path must not create a replacement key");
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
    fn batch_delete_removes_requested_references_and_preserves_other_authorities() {
        let _backend = enable_insecure_test_backend();
        let first = stable_reference("queue-item", "first").expect("first reference");
        let second = stable_reference("queue-item", "second").expect("second reference");
        let retained = stable_reference("config-a", "shared").expect("retained reference");
        set(&first, "first-secret").expect("store first");
        set(&second, "second-secret").expect("store second");
        set(&retained, "retained-secret").expect("store retained");

        delete_many_if_present(&[first.clone(), second.clone(), first.clone()])
            .expect("idempotent batch retirement");

        assert_eq!(insecure_test_secret_count(), 1);
        assert!(get(&first).is_err());
        assert!(get(&second).is_err());
        assert_eq!(get(&retained).as_deref(), Ok("retained-secret"));
        delete_many_if_present(&[first, second]).expect("repeat delete is idempotent");
        assert_eq!(insecure_test_secret_count(), 1);
    }

    #[test]
    fn headless_pending_publication_retirement_validates_then_reports_orphan_risk() {
        let _backend = enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = temp.path().join("config.toml");
        let unpublished = stable_reference("config-a", "unpublished").expect("reference");
        let superseded = stable_reference("config-b", "superseded").expect("reference");
        set(&unpublished, "new-secret").expect("store unpublished secret");
        set(&superseded, "old-secret").expect("store superseded secret");
        begin_pending_publication_with_retirement(
            &publication,
            std::slice::from_ref(&unpublished),
            std::slice::from_ref(&superseded),
        )
        .expect("publish pending journal");

        let at_risk = retire_pending_publication_journal_headless(&publication)
            .expect("headless retirement");

        assert_eq!(at_risk, 2);
        assert!(!pending_publication_path(&publication).exists());
        assert_eq!(get(&unpublished).as_deref(), Ok("new-secret"));
        assert_eq!(get(&superseded).as_deref(), Ok("old-secret"));
    }

    #[test]
    fn headless_pending_publication_retirement_refuses_malformed_journal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = temp.path().join("config.toml");
        let journal = pending_publication_path(&publication);
        std::fs::write(&journal, b"{not-json").expect("write malformed journal");

        let error = retire_pending_publication_journal_headless(&publication)
            .expect_err("malformed journal must remain authoritative");

        assert!(error.contains(&format!("parse pending secret-publication journal '{}'", journal.display())));
        assert_eq!(std::fs::read(&journal).expect("journal retained"), b"{not-json");
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
