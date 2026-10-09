//! Secure storage provided by the app, for platforms where the core has no
//! OS credential store of its own. On Android the app encrypts values with
//! an Android Keystore key. Registered as keyring-core's default store, so
//! the core's `Auth` uses it like the iOS Keychain or Windows Credential
//! Manager.

use std::any::Any;
use std::sync::Arc;

use keyring_core::api::{CredentialApi, CredentialStoreApi};
use keyring_core::{Credential, Entry, Error};

use crate::FfiError;

#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    fn get(&self, key: String) -> Result<Option<String>, FfiError>;
    fn set(&self, key: String, value: String) -> Result<(), FfiError>;
    fn delete(&self, key: String) -> Result<(), FfiError>;
}

/// Makes `store` hold the sign-in token. Call before creating `YtLite`.
#[uniffi::export]
pub fn set_secret_store(store: Arc<dyn SecretStore>) {
    keyring_core::set_default_store(Arc::new(Store(store)));
}

struct Store(Arc<dyn SecretStore>);

impl CredentialStoreApi for Store {
    fn vendor(&self) -> String {
        "yt-lite app-provided store".into()
    }

    fn id(&self) -> String {
        "yt-lite".into()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        _modifiers: Option<&std::collections::HashMap<&str, &str>>,
    ) -> keyring_core::Result<Entry> {
        Ok(Entry::new_with_credential(Arc::new(Cred {
            store: self.0.clone(),
            key: format!("{service}/{user}"),
            specifiers: (service.into(), user.into()),
        })))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Cred {
    store: Arc<dyn SecretStore>,
    key: String,
    specifiers: (String, String),
}

fn platform(e: FfiError) -> Error {
    Error::PlatformFailure(Box::new(e))
}

impl CredentialApi for Cred {
    fn set_secret(&self, secret: &[u8]) -> keyring_core::Result<()> {
        let value =
            String::from_utf8(secret.to_vec()).map_err(|e| Error::BadEncoding(e.into_bytes()))?;
        self.store.set(self.key.clone(), value).map_err(platform)
    }

    fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
        match self.store.get(self.key.clone()).map_err(platform)? {
            Some(value) => Ok(value.into_bytes()),
            None => Err(Error::NoEntry),
        }
    }

    fn delete_credential(&self) -> keyring_core::Result<()> {
        self.get_secret()?;
        self.store.delete(self.key.clone()).map_err(platform)
    }

    fn get_credential(&self) -> keyring_core::Result<Option<Arc<Credential>>> {
        self.get_secret()?;
        Ok(None)
    }

    fn get_specifiers(&self) -> Option<(String, String)> {
        Some(self.specifiers.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
