//! Keeps the Bluetooth bonds (the keys of paired phones) in the encrypted secret store.
//!
//! NimBLE can save bonds itself, but it writes them in plain text to the `nvs` partition, which
//! flash encryption does not cover. So NimBLE keeps its bonds in RAM only, and this module copies
//! them to and from the [`SecretStore`] (see `docs/bluetooth.md`).
//!
//! # Format
//!
//! One secret, `config::BLE_BONDS_SECRET_KEY`:
//!
//! ```text
//! "PBND" | version (1 byte) | record*
//! record = kind (1 byte, BondObject::id) | length (2 bytes, little endian) | bytes
//! ```
//!
//! The bytes are NimBLE's C structs as they are in memory. That is why every record carries its
//! length: if an ESP-IDF update changes a struct, the length no longer matches
//! [`BondObject::size`], the whole list is dropped and the phones pair again. Loading old bytes
//! into a new layout would hand the stack garbage keys.

use esp_idf_svc::sys::EspError;
use zeroize::Zeroizing;

use crate::config::BLE_BONDS_SECRET_KEY;
use crate::platform::{self, BondObject};
use crate::secrets::{self, SecretStore};

/// Marks the secret as a bond list.
const MAGIC: [u8; 4] = *b"PBND";
/// Changes when this format changes.
const VERSION: u8 = 1;

/// What can go wrong when saving the bonds.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// NimBLE could not hand over its records.
    #[error("cannot read the bonds from the Bluetooth stack: {0}")]
    Stack(#[from] EspError),
    /// The secret store failed.
    #[error("secret store: {0}")]
    Secrets(#[from] secrets::Error),
}

/// Moves the bonds between NimBLE and the secret store.
pub struct BondStore {
    /// `None` when the secret store is not available: bonds then live in RAM only.
    secrets: Option<SecretStore>,
}

impl BondStore {
    /// A bond store on `secrets`, which may be missing (see [`SecretStore::open`]).
    #[must_use]
    pub fn new(secrets: Option<SecretStore>) -> Self {
        if secrets.is_none() {
            log::warn!("no secret store: Bluetooth bonds are kept in RAM and lost at reboot");
        }
        Self { secrets }
    }

    /// Loads the saved bonds into NimBLE. Problems are logged and never stop the start-up: the
    /// worst case is that phones have to pair again.
    pub fn restore(&self) {
        let Some(secrets) = &self.secrets else {
            return;
        };
        let blob = match secrets.get(BLE_BONDS_SECRET_KEY) {
            Ok(Some(blob)) => blob,
            Ok(None) => return,
            Err(error) => {
                log::warn!("cannot read the saved Bluetooth bonds: {error}");
                return;
            }
        };

        // Check the whole list first, so a stale one is dropped as a whole and never half-loaded.
        let records = match decode(&blob) {
            Ok(records) => records,
            Err(reason) => {
                log::warn!("saved Bluetooth bonds dropped ({reason}); phones must pair again");
                if let Err(error) = secrets.erase(BLE_BONDS_SECRET_KEY) {
                    log::warn!("cannot erase the stale bond list: {error}");
                }
                return;
            }
        };
        for (object, bytes) in &records {
            if let Err(error) = platform::ble_bonds_import(*object, bytes) {
                log::warn!("cannot load a saved Bluetooth bond record: {error}");
            }
        }
        log::info!("{} saved Bluetooth bond records loaded", records.len());
    }

    /// Saves NimBLE's current bonds. An empty list erases the saved one.
    ///
    /// # Errors
    ///
    /// Fails if NimBLE cannot be read or the secret store cannot be written.
    pub fn save(&self) -> Result<(), Error> {
        let Some(secrets) = &self.secrets else {
            return Ok(());
        };

        // The full capacity up front, so the buffer never moves and leaves no key material behind.
        let mut blob = Zeroizing::new(Vec::with_capacity(secrets::MAX_SECRET_LEN));
        blob.extend_from_slice(&MAGIC);
        blob.push(VERSION);
        let header_len = blob.len();

        platform::ble_bonds_export(|object, bytes| {
            // A record is a few hundred bytes at most, so its length always fits in two bytes.
            let length = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
            blob.push(object.id());
            blob.extend_from_slice(&length.to_le_bytes());
            blob.extend_from_slice(bytes);
        })?;

        if blob.len() == header_len {
            secrets.erase(BLE_BONDS_SECRET_KEY)?;
        } else {
            secrets.put(BLE_BONDS_SECRET_KEY, &blob)?;
        }
        Ok(())
    }
}

/// Splits a saved bond list into its records.
///
/// # Errors
///
/// Returns why the list cannot be used: another format or version, damage, or records written
/// for another NimBLE layout.
fn decode(blob: &[u8]) -> Result<Vec<(BondObject, &[u8])>, &'static str> {
    let rest = blob.strip_prefix(&MAGIC).ok_or("unknown format")?;
    let (&version, mut rest) = rest.split_first().ok_or("truncated")?;
    if version != VERSION {
        return Err("other format version");
    }

    let mut records = Vec::new();
    while let Some((&id, tail)) = rest.split_first() {
        let object = BondObject::from_id(id).ok_or("unknown record kind")?;
        let (length, tail) = tail.split_first_chunk::<2>().ok_or("truncated")?;
        let length = usize::from(u16::from_le_bytes(*length));
        let (bytes, tail) = tail.split_at_checked(length).ok_or("truncated")?;
        if length != object.size() {
            return Err("the Bluetooth stack has another record layout now");
        }
        records.push((object, bytes));
        rest = tail;
    }
    Ok(records)
}
