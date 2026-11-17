//! The one crate that talks to the OS secret store (macOS Keychain, Windows Credential Manager,
//! Secret Service), used by the desktop, `rustgis-cli --use-saved-secrets` and Python's
//! `use_saved_secrets`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::{Arc, OnceLock};

use rustgis_data::secrets::{SERVICE, Secret, SecretError, SecretStore, check_account};

/// The OS credential store, opened on the first call: constructing this hook never touches
/// D-Bus or triggers a prompt. Callers such as Assistant key migration use a worker thread.
#[derive(Default)]
pub struct Keychain {
    store: OnceLock<Result<Arc<keyring_core::CredentialStore>, String>>,
}

#[cfg(target_os = "macos")]
fn open() -> Result<Arc<keyring_core::CredentialStore>, String> {
    let store: Arc<keyring_core::CredentialStore> = apple_native_keyring_store::keychain::Store::new().map_err(|e| e.to_string())?;
    Ok(store)
}

#[cfg(windows)]
fn open() -> Result<Arc<keyring_core::CredentialStore>, String> {
    let store: Arc<keyring_core::CredentialStore> = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
    Ok(store)
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
fn open() -> Result<Arc<keyring_core::CredentialStore>, String> {
    let store: Arc<keyring_core::CredentialStore> = zbus_secret_service_keyring_store::Store::new().map_err(|e| e.to_string())?;
    Ok(store)
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn open() -> Result<Arc<keyring_core::CredentialStore>, String> {
    Err("no system keychain on this platform".into())
}

/// Keeps a credential on this computer instead of the default roaming `Enterprise` persistence.
#[cfg(windows)]
fn build(store: &keyring_core::CredentialStore, account: &str) -> keyring_core::Result<keyring_core::Entry> {
    store.build(SERVICE, account, Some(&std::collections::HashMap::from([("persistence", "Local")])))
}

#[cfg(not(windows))]
fn build(store: &keyring_core::CredentialStore, account: &str) -> keyring_core::Result<keyring_core::Entry> {
    store.build(SERVICE, account, None)
}

impl Keychain {
    fn entry(&self, account: &str) -> Result<keyring_core::Entry, SecretError> {
        check_account(account)?;
        match self.store.get_or_init(open) {
            Ok(store) => build(&**store, account).map_err(map_error),
            Err(why) => Err(SecretError::Unavailable(why.clone())),
        }
    }
}

/// Never formats a payload that holds a secret's bytes. `NoEntry` is for the callers to handle.
fn map_error(e: keyring_core::Error) -> SecretError {
    use keyring_core::Error;
    match e {
        Error::NoStorageAccess(_) => SecretError::Denied,
        Error::NoDefaultStore => SecretError::Unavailable("no credential store".into()),
        Error::PlatformFailure(err) => SecretError::Failed(err.to_string()),
        Error::BadEncoding(_) | Error::BadDataFormat(..) => SecretError::Failed("the stored value isn't text".into()),
        e => SecretError::Failed(e.to_string()),
    }
}

impl SecretStore for Keychain {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        self.entry(account)?.set_password(value.expose()).map_err(map_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_never_show_the_secret() {
        let e = keyring_core::Error::BadEncoding(b"hunter2".to_vec());
        let mapped = map_error(e);
        assert!(!mapped.to_string().contains("hunter2"));
        assert!(!format!("{mapped:?}").contains("hunter2"));
        let denied = map_error(keyring_core::Error::NoStorageAccess("locked".into()));
        assert_eq!(denied, SecretError::Denied);
    }

    #[test]
    fn bad_accounts_never_reach_the_store() {
        assert_eq!(Keychain::default().load("bad"), Err(SecretError::InvalidAccount("bad".into())));
    }

    #[test]
    #[ignore = "needs the system keychain; run with --ignored"]
    fn keychain_round_trip() {
        let keychain = Keychain::default();
        let account = "test/keychain-smoke";
        keychain.delete(account).unwrap();
        keychain.save(account, &Secret::new("rustgis-smoke-1")).unwrap();
        assert_eq!(keychain.load(account).unwrap(), Some(Secret::new("rustgis-smoke-1")));
        keychain.save(account, &Secret::new("rustgis-smoke-2")).unwrap();
        assert_eq!(keychain.load(account).unwrap(), Some(Secret::new("rustgis-smoke-2")));
        keychain.delete(account).unwrap();
        assert_eq!(keychain.load(account).unwrap(), None);
    }
}
