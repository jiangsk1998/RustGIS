//! Opaque secrets and stores shared by desktop and wasm callers.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Mutex, OnceLock},
};

/// The service name used by every RustGIS secret store.
pub const SERVICE: &str = "RustGIS";

/// A secret whose debug output never reveals its value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Store failures never carry a secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    Unavailable(String),
    Denied,
    InvalidAccount(String),
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "the system keychain isn't available: {why}"),
            Self::Denied => f.write_str("the system keychain is locked or access to it was refused"),
            Self::InvalidAccount(account) => write!(f, "invalid secret name \"{account}\""),
            Self::Failed(why) => write!(f, "the system keychain failed: {why}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Accepts `owner/name`, with a lowercase ASCII owner and at most 200 bytes, without controls.
pub fn check_account(account: &str) -> Result<(), SecretError> {
    let valid = account.len() <= 200
        && !account.chars().any(char::is_control)
        && account.split_once('/').is_some_and(|(owner, name)| {
            !owner.is_empty() && !name.is_empty() && owner.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    if valid {
        return Ok(());
    }
    let mut end = account.len().min(200);
    while !account.is_char_boundary(end) {
        end -= 1;
    }
    Err(SecretError::InvalidAccount(account.get(..end).unwrap_or_default().to_string()))
}

/// A store for secrets by account (`owner/name`), under [`SERVICE`]. Implementations call
/// [`check_account`] first and never write a value anywhere but their store.
pub trait SecretStore: Send + Sync {
    /// The secret, or `None` when none is stored for `account`.
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError>;
    /// Stores `value` for `account`, replacing any earlier one.
    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError>;
    /// Forgets `account`'s secret; succeeds when there was none.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// Calls `warn` once when the keychain fails (not for an invalid account), and stops asking an
/// unavailable store. Refusals remain per item, so a denied item does not disable other items.
pub struct Reporting<S: SecretStore> {
    inner: S,
    warn: Box<dyn Fn(&SecretError) + Send + Sync>,
    warned: std::sync::atomic::AtomicBool,
    gone: OnceLock<SecretError>,
}

impl<S: SecretStore> Reporting<S> {
    pub fn new(inner: S, warn: impl Fn(&SecretError) + Send + Sync + 'static) -> Self {
        Self { inner, warn: Box::new(warn), warned: std::sync::atomic::AtomicBool::new(false), gone: OnceLock::new() }
    }

    fn call<T>(&self, account: &str, operation: impl FnOnce(&S) -> Result<T, SecretError>) -> Result<T, SecretError> {
        check_account(account)?;
        if let Some(error) = self.gone.get() {
            return Err(error.clone());
        }
        let result = operation(&self.inner);
        if let Err(error) = &result
            && !matches!(error, SecretError::InvalidAccount(_))
        {
            if !self.warned.swap(true, std::sync::atomic::Ordering::SeqCst) {
                (self.warn)(error);
            }
            if matches!(error, SecretError::Unavailable(_)) {
                let _ = self.gone.set(error.clone());
            }
        }
        result
    }
}

impl<S: SecretStore> SecretStore for Reporting<S> {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        self.call(account, |inner| inner.load(account))
    }

    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        self.call(account, |inner| inner.save(account, value))
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.call(account, |inner| inner.delete(account))
    }
}

/// An in-memory store for tests of code that uses secrets; never a fallback for the OS store.
#[derive(Default)]
pub struct MemorySecrets {
    values: Mutex<BTreeMap<String, String>>,
    fail: Option<SecretError>,
}

impl MemorySecrets {
    pub fn new() -> MemorySecrets {
        Self::default()
    }

    /// A store whose every call fails, after validating the account.
    pub fn failing(error: SecretError) -> MemorySecrets {
        Self { fail: Some(error), ..Self::default() }
    }
}

impl SecretStore for MemorySecrets {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        check_account(account)?;
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        Ok(self.values.lock().unwrap_or_else(|e| e.into_inner()).get(account).cloned().map(Secret::new))
    }

    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        check_account(account)?;
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        self.values.lock().unwrap_or_else(|e| e.into_inner()).insert(account.to_string(), value.expose().to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        check_account(account)?;
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        self.values.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };

    struct Counting {
        inner: MemorySecrets,
        loads: Arc<AtomicUsize>,
    }

    impl SecretStore for Counting {
        fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
            self.loads.fetch_add(1, SeqCst);
            self.inner.load(account)
        }
        fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
            self.inner.save(account, value)
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.inner.delete(account)
        }
    }

    #[test]
    fn reporting_unavailable_stops_but_denied_retries() {
        for (error, expected_loads) in [(SecretError::Unavailable("payload".into()), 1), (SecretError::Denied, 2)] {
            let loads = Arc::new(AtomicUsize::new(0));
            let warnings = Arc::new(AtomicUsize::new(0));
            let count = warnings.clone();
            let store = Reporting::new(Counting { inner: MemorySecrets::failing(error.clone()), loads: loads.clone() }, move |_| {
                count.fetch_add(1, SeqCst);
            });
            for _ in 0..2 {
                assert_eq!(store.load("test/item"), Err(error.clone()));
            }
            assert_eq!(warnings.load(SeqCst), 1);
            assert_eq!(loads.load(SeqCst), expected_loads);
        }
    }

    #[test]
    fn reporting_success_and_invalid_accounts_do_not_warn() {
        let loads = Arc::new(AtomicUsize::new(0));
        let warnings = Arc::new(AtomicUsize::new(0));
        let count = warnings.clone();
        let store = Reporting::new(Counting { inner: MemorySecrets::new(), loads: loads.clone() }, move |_| {
            count.fetch_add(1, SeqCst);
        });
        assert_eq!(store.load("bad"), Err(SecretError::InvalidAccount("bad".into())));
        assert_eq!(loads.load(SeqCst), 0);
        let secret = Secret::new("round-trip");
        store.save("test/item", &secret).unwrap();
        assert_eq!(store.load("test/item").unwrap(), Some(secret));
        store.delete("test/item").unwrap();
        assert_eq!(store.load("test/item").unwrap(), None);
        assert_eq!(warnings.load(SeqCst), 0);
    }

    #[test]
    fn account_rules() {
        for account in ["assistant/openai", "connection/3f2a-9b", "a/nested/name", "a/é"] {
            assert_eq!(check_account(account), Ok(()));
        }
        for account in ["", "assistant", "/x", "assistant/", "Assistant/x", "a/b\n", "a/b\u{7f}"] {
            assert_eq!(check_account(account), Err(SecretError::InvalidAccount(account.into())));
        }
        assert!(check_account(&format!("a/{}", "x".repeat(198))).is_ok());
        let long = format!("a/{}", "x".repeat(199));
        assert_eq!(check_account(&long), Err(SecretError::InvalidAccount(long.get(..200).unwrap().into())));
        let unicode = format!("a/{}é", "x".repeat(197));
        assert_eq!(check_account(&unicode), Err(SecretError::InvalidAccount(format!("a/{}", "x".repeat(197)))));
    }

    #[test]
    fn round_trip() {
        let store = MemorySecrets::new();
        let account = "assistant/openai";
        assert_eq!(store.load(account).unwrap(), None);
        store.save(account, &Secret::new("first")).unwrap();
        assert_eq!(store.load(account).unwrap(), Some(Secret::new("first")));
        store.save(account, &Secret::new("second")).unwrap();
        assert_eq!(store.load(account).unwrap(), Some(Secret::new("second")));
        store.delete(account).unwrap();
        assert_eq!(store.load(account).unwrap(), None);
        assert_eq!(store.delete(account), Ok(()));
    }

    #[test]
    fn accounts_are_independent() {
        let store = MemorySecrets::new();
        store.save("assistant/openai", &Secret::new("one")).unwrap();
        store.save("connection/3f2a-9b", &Secret::new("two")).unwrap();
        store.delete("assistant/openai").unwrap();
        assert_eq!(store.load("assistant/openai").unwrap(), None);
        assert_eq!(store.load("connection/3f2a-9b").unwrap(), Some(Secret::new("two")));
    }

    #[test]
    fn failing_store_validates_first() {
        let store = MemorySecrets::failing(SecretError::Denied);
        assert_eq!(store.load("assistant/openai"), Err(SecretError::Denied));
        assert_eq!(store.save("assistant/openai", &Secret::new("value")), Err(SecretError::Denied));
        assert_eq!(store.delete("assistant/openai"), Err(SecretError::Denied));
        let invalid = SecretError::InvalidAccount("bad".into());
        assert_eq!(store.load("bad"), Err(invalid.clone()));
        assert_eq!(store.save("bad", &Secret::new("value")), Err(invalid.clone()));
        assert_eq!(store.delete("bad"), Err(invalid));
    }

    #[test]
    fn secret_debug_is_redacted() {
        assert!(!format!("{:?}", Secret::new("hunter2")).contains("hunter2"));
    }
}
