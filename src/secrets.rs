//! Storage for small secrets (for example a Wi-Fi password) on the chip.
//!
//! Secrets live in an **encrypted NVS** partition (`secrets`). ESP-IDF's NVS provides the record
//! format, integrity checks and wear levelling; this module only adds the safety checks and a
//! small byte-oriented API.
//!
//! # How the secret is protected
//!
//! The NVS encryption keys are kept in the `nvs_keys` partition, which is protected by the
//! chip's **flash encryption** (an AES key in eFuses that software cannot read). So the secret
//! is safe from a dump of the flash chip only once flash encryption is turned on; see
//! `docs/secure-wifi-secrets.md`. Until then [`SecretStore::open`] refuses to open and returns
//! [`Error::NotProtected`], **before** any key is generated. The Cargo feature
//! `insecure-dev-store` lets it open anyway (the keys are then readable from flash) and logs a
//! warning, so the same code path can be tested before the eFuses are burned.
//!
//! What this does **not** protect against: code running on the chip itself (it can read the
//! secret), a copy kept elsewhere (shell history, build output on your computer), and copies in
//! RAM while the secret is in use. Secrets returned by [`SecretStore::get`] are wiped when dropped.
//!
//! Secrets are never logged by this module.

use esp_idf_svc::nvs::{EspEncryptedNvsPartition, EspNvs, NvsEncrypted};
use esp_idf_svc::sys::EspError;
use zeroize::Zeroizing;

use crate::platform;

/// Label of the encrypted NVS partition (see `partitions.csv`).
const PARTITION: &str = "secrets";
/// Label of the partition that holds the NVS encryption keys.
const KEYS_PARTITION: &str = "nvs_keys";
/// NVS namespace of all secrets.
const NAMESPACE: &str = "pulsar";

/// Longest secret in bytes.
pub const MAX_SECRET_LEN: usize = 256;

/// What can go wrong with the secret store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Flash encryption is off, so the secrets would not be protected.
    #[error(
        "refusing to store secrets: flash encryption is not enabled \
         (see docs/secure-wifi-secrets.md, or build with --features insecure-dev-store to test)"
    )]
    NotProtected,
    /// The secret is longer than [`MAX_SECRET_LEN`].
    #[error("secret is longer than {MAX_SECRET_LEN} bytes")]
    TooLong,
    /// The stored data does not look like something this module wrote.
    #[error("stored secret is damaged")]
    Corrupt,
    /// The NVS layer failed.
    #[error("storage error: {0}")]
    Storage(#[from] EspError),
}

/// A place where small secrets can be kept, addressed by a key name (at most 15 characters).
pub struct SecretStore {
    nvs: EspNvs<NvsEncrypted>,
}

impl SecretStore {
    /// Opens the encrypted `secrets` partition. Call this once.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotProtected`] unless flash encryption is enabled (or the
    /// `insecure-dev-store` feature is on), and [`Error::Storage`] if the partitions are missing.
    pub fn open() -> Result<Self, Error> {
        // This check must come first: opening the partition generates the keys if there are none.
        if platform::flash_encryption_enabled() {
            log::info!("flash encryption is on, the NVS keys are protected");
        } else if cfg!(feature = "insecure-dev-store") {
            log::warn!(
                "INSECURE dev mode: flash encryption is off, so anyone with the flash \
                 contents can read the secrets"
            );
        } else {
            return Err(Error::NotProtected);
        }

        let partition = EspEncryptedNvsPartition::take(PARTITION, Some(KEYS_PARTITION))?;
        let nvs = EspNvs::new(partition, NAMESPACE, true)?;
        Ok(Self { nvs })
    }

    /// Reads the secret stored under `key`, or `None` if there is none.
    ///
    /// # Errors
    ///
    /// Fails if the storage cannot be read or the stored data is damaged.
    pub fn get(&self, key: &str) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
        let Some(len) = self.nvs.blob_len(key)? else {
            return Ok(None);
        };
        if len > MAX_SECRET_LEN {
            return Err(Error::Corrupt);
        }

        let mut secret = Zeroizing::new(vec![0; len]);
        let read = self.nvs.get_blob(key, &mut secret)?.map(<[u8]>::len);
        Ok(read.map(|read| {
            secret.truncate(read);
            secret
        }))
    }

    /// Stores `secret` under `key`, replacing what was there. Does nothing if it is already
    /// stored (this saves flash wear).
    ///
    /// # Errors
    ///
    /// Fails if the secret is too long or the storage cannot be written.
    pub fn put(&self, key: &str, secret: &[u8]) -> Result<(), Error> {
        if secret.len() > MAX_SECRET_LEN {
            return Err(Error::TooLong);
        }
        if self
            .get(key)?
            .is_some_and(|current| current.as_slice() == secret)
        {
            return Ok(());
        }
        self.nvs.set_blob(key, secret)?;
        Ok(())
    }

    /// Deletes the secret under `key`. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// Fails if the storage cannot be written.
    pub fn erase(&self, key: &str) -> Result<bool, Error> {
        Ok(self.nvs.remove(key)?)
    }

    /// Writes, reads back and deletes a test secret and logs the outcome (never the secret).
    /// For checking the storage on a new board.
    ///
    /// # Errors
    ///
    /// Fails if the storage misbehaves.
    #[cfg(feature = "insecure-dev-store")]
    pub fn self_test(&self) -> Result<(), Error> {
        const KEY: &str = "selftest";
        const VALUE: &[u8] = b"self-test-secret";

        self.put(KEY, VALUE)?;
        let matches = self
            .get(KEY)?
            .is_some_and(|stored| stored.as_slice() == VALUE);
        log::info!(
            "write + read back {}",
            if matches { "OK" } else { "MISMATCH" }
        );

        let erased = self.erase(KEY)? && self.get(KEY)?.is_none();
        log::info!("erase {}", if erased { "OK" } else { "FAILED" });
        Ok(())
    }
}
