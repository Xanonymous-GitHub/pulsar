//! Storage for small secrets (for example a Wi-Fi password) on the chip.
//!
//! The module has two layers:
//!
//! - [`SecretStore`] is the interface the rest of the firmware uses: `get`, `put` and `erase`
//!   by slot number. [`SlotStore`] implements it on top of any [`TableBackend`], with a small
//!   checked record format (magic, version, slot, length, CRC-32).
//! - [`EncryptedFlash`] is the backend for the real chip. It keeps all slots in one 4 KiB sector
//!   of a dedicated flash partition named [`PARTITION_LABEL`] that is flagged `encrypted`
//!   (see `partitions.csv`).
//!
//! # How the secret is protected
//!
//! The chip's **flash encryption** (an AES key burned into eFuses that software cannot read)
//! encrypts the partition, so a dump of the flash chip does not contain the secret in plain text.
//! This is only true once flash encryption is enabled on the chip; see
//! `docs/secure-wifi-secrets.md`. Until then [`EncryptedFlash::open`] **refuses to open** the
//! partition and returns [`Error::NotEncrypted`]. Enabling the Cargo feature `insecure-dev-store`
//! lets it open anyway (the secret is then stored in plain text) and logs a warning on every
//! boot. Use that only for testing.
//!
//! What this does **not** protect against: code running on the chip itself (it can read the
//! secret), a copy of the secret kept elsewhere (shell history, the build output on your
//! computer), and copies in RAM while the secret is in use. Our own buffers are wiped after use
//! on a best-effort basis.
//!
//! Secrets are never logged by this module.

use core::fmt;
use core::sync::atomic::{Ordering, compiler_fence};

use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_storage::{FlashStorage, FlashStorageError};

/// Label of the flash partition that holds the secrets.
pub const PARTITION_LABEL: &str = "secrets";
/// Number of secret slots (numbered `0..SLOT_COUNT`).
pub const SLOT_COUNT: usize = 8;
/// Size of one stored record, in bytes.
pub const RECORD_LEN: usize = 128;
/// Largest secret that fits in one slot, in bytes.
pub const MAX_SECRET_LEN: usize = RECORD_LEN - HEADER_LEN - CRC_LEN;

/// Size of the table that holds all records, in bytes.
pub const TABLE_LEN: usize = SLOT_COUNT * RECORD_LEN;
/// The `encrypted` bit in a partition table entry's flags, as defined by ESP-IDF (bit 0; bit 1 is
/// `readonly`). `esp-bootloader-esp-idf` 0.6.0 has the two bits swapped (its `is_encrypted()` reads
/// bit 1 and `is_read_only()` bit 0), so we read the raw flags and do the flash access ourselves
/// instead of using its `as_flash_region`, which would report `WriteProtected` for this partition.
const PART_FLAG_ENCRYPTED: u32 = 1 << 0;
const SECTOR_LEN: usize = FlashStorage::SECTOR_SIZE as usize;
const MAGIC: [u8; 4] = *b"PSEC";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 8;
const CRC_LEN: usize = 4;

/// Everything that can go wrong when using a secret store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The slot number is not below [`SLOT_COUNT`].
    InvalidSlot,
    /// The secret is longer than [`MAX_SECRET_LEN`].
    TooLong,
    /// The buffer given to `get` is smaller than the stored secret.
    BufferTooSmall,
    /// A record exists but its contents do not pass the checks (wrong slot, length or CRC).
    Corrupt,
    /// The partition table has no partition labelled [`PARTITION_LABEL`].
    PartitionNotFound,
    /// The partition is smaller than one flash sector (4 KiB).
    PartitionTooSmall,
    /// The partition is not encrypted, so the store refuses to keep a secret in it.
    NotEncrypted {
        /// Whether flash encryption is enabled in the chip's eFuses.
        flash_encryption_enabled: bool,
        /// Whether the partition has the `encrypted` flag in the partition table.
        partition_flagged: bool,
    },
    /// Reading the partition table or the flash failed.
    Storage(partitions::Error),
    Flash(FlashStorageError),
}

impl From<FlashStorageError> for Error {
    fn from(error: FlashStorageError) -> Self {
        Self::Flash(error)
    }
}

impl From<partitions::Error> for Error {
    fn from(error: partitions::Error) -> Self {
        Self::Storage(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSlot => write!(f, "invalid secret slot"),
            Self::TooLong => write!(f, "secret is longer than {MAX_SECRET_LEN} bytes"),
            Self::BufferTooSmall => write!(f, "output buffer is too small for the secret"),
            Self::Corrupt => write!(f, "stored secret record is corrupt"),
            Self::PartitionNotFound => write!(
                f,
                "no '{PARTITION_LABEL}' partition: flash with `--partition-table partitions.csv`"
            ),
            Self::PartitionTooSmall => {
                write!(f, "the '{PARTITION_LABEL}' partition is under 4 KiB")
            }
            Self::NotEncrypted {
                flash_encryption_enabled,
                partition_flagged,
            } => write!(
                f,
                "refusing to store secrets unencrypted (flash encryption enabled: \
                 {flash_encryption_enabled}, partition flagged encrypted: {partition_flagged}); \
                 see docs/secure-wifi-secrets.md"
            ),
            Self::Storage(error) => write!(f, "partition table error: {error:?}"),
            Self::Flash(error) => write!(f, "flash error: {error:?}"),
        }
    }
}

impl core::error::Error for Error {}

/// A small secret that is wiped from memory when dropped.
///
/// `Debug` never prints the contents.
pub struct Secret<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Secret<N> {
    /// Copies `data` into a new secret. Returns `None` if it is longer than `N` bytes.
    pub fn from_slice(data: &[u8]) -> Option<Self> {
        if data.len() > N {
            return None;
        }
        let mut bytes = [0; N];
        bytes[..data.len()].copy_from_slice(data);
        Some(Self {
            bytes,
            len: data.len(),
        })
    }

    /// Joins `parts` into one new secret. Returns `None` if the total is longer than `N` bytes.
    pub fn concat(parts: &[&[u8]]) -> Option<Self> {
        let mut secret = Self {
            bytes: [0; N],
            len: 0,
        };
        for part in parts {
            let end = secret.len.checked_add(part.len())?;
            secret.bytes.get_mut(secret.len..end)?.copy_from_slice(part);
            secret.len = end;
        }
        Some(secret)
    }

    /// The secret bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Length of the secret in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the secret is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        wipe(&mut self.bytes);
    }
}

impl<const N: usize> fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<redacted, {} bytes>)", self.len)
    }
}

/// Overwrites `bytes` with zeros in a way the compiler may not optimize away.
fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, exclusive reference to an initialized `u8`.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// A word-aligned scratch buffer that is wiped when dropped. Used for data that may hold secrets.
#[repr(align(4))]
struct Scratch<const N: usize>([u8; N]);

impl<const N: usize> Scratch<N> {
    const fn new() -> Self {
        Self([0; N])
    }
}

impl<const N: usize> Drop for Scratch<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// A place where small secrets can be kept, addressed by slot number.
pub trait SecretStore {
    /// Copies the secret in `slot` into `out` and returns its length, or `None` if the slot is
    /// empty.
    fn get(&mut self, slot: u8, out: &mut [u8]) -> Result<Option<usize>, Error>;

    /// Stores `secret` in `slot`, replacing what was there. Does nothing if the slot already
    /// holds exactly this secret (this saves flash wear).
    fn put(&mut self, slot: u8, secret: &[u8]) -> Result<(), Error>;

    /// Empties `slot`.
    fn erase(&mut self, slot: u8) -> Result<(), Error>;

    /// Like [`get`](Self::get), but returns the secret in a [`Secret`] that wipes itself when
    /// dropped. Fails with [`Error::BufferTooSmall`] if the stored secret is longer than `N`.
    fn get_secret<const N: usize>(&mut self, slot: u8) -> Result<Option<Secret<N>>, Error>
    where
        Self: Sized,
    {
        let mut secret = Secret::<N> {
            bytes: [0; N],
            len: 0,
        };
        Ok(self.get(slot, &mut secret.bytes)?.map(|len| {
            secret.len = len;
            secret
        }))
    }
}

/// Raw access to the table of [`SLOT_COUNT`] records. This is what a storage medium implements.
pub trait TableBackend {
    /// Reads the whole table.
    fn read_table(&mut self, table: &mut [u8; TABLE_LEN]) -> Result<(), Error>;

    /// Replaces the whole table.
    fn write_table(&mut self, table: &[u8; TABLE_LEN]) -> Result<(), Error>;
}

/// Implements [`SecretStore`] on top of a [`TableBackend`].
pub struct SlotStore<B> {
    backend: B,
}

impl<B: TableBackend> SlotStore<B> {
    /// Wraps a backend.
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }

    fn read(&mut self) -> Result<Scratch<TABLE_LEN>, Error> {
        let mut table = Scratch::<TABLE_LEN>::new();
        self.backend.read_table(&mut table.0)?;
        Ok(table)
    }
}

impl<B: TableBackend> SecretStore for SlotStore<B> {
    fn get(&mut self, slot: u8, out: &mut [u8]) -> Result<Option<usize>, Error> {
        check_slot(slot)?;
        let table = self.read()?;
        match decode_record(slot, record(&table.0, slot))? {
            None => Ok(None),
            Some(data) => {
                let out = out.get_mut(..data.len()).ok_or(Error::BufferTooSmall)?;
                out.copy_from_slice(data);
                Ok(Some(out.len()))
            }
        }
    }

    fn put(&mut self, slot: u8, secret: &[u8]) -> Result<(), Error> {
        check_slot(slot)?;
        let mut table = self.read()?;
        let mut new_record = Scratch::<RECORD_LEN>::new();
        encode_record(slot, secret, &mut new_record.0)?;
        if record(&table.0, slot) == new_record.0 {
            return Ok(());
        }
        record_mut(&mut table.0, slot).copy_from_slice(&new_record.0);
        self.backend.write_table(&table.0)
    }

    fn erase(&mut self, slot: u8) -> Result<(), Error> {
        check_slot(slot)?;
        let mut table = self.read()?;
        let current = record_mut(&mut table.0, slot);
        if current.iter().all(|byte| *byte == 0xFF) {
            return Ok(());
        }
        current.fill(0xFF);
        self.backend.write_table(&table.0)
    }
}

fn check_slot(slot: u8) -> Result<(), Error> {
    if usize::from(slot) < SLOT_COUNT {
        Ok(())
    } else {
        Err(Error::InvalidSlot)
    }
}

fn record(table: &[u8; TABLE_LEN], slot: u8) -> &[u8] {
    let start = usize::from(slot) * RECORD_LEN;
    &table[start..start + RECORD_LEN]
}

fn record_mut(table: &mut [u8; TABLE_LEN], slot: u8) -> &mut [u8] {
    let start = usize::from(slot) * RECORD_LEN;
    &mut table[start..start + RECORD_LEN]
}

/// Record layout: magic (4) | version (1) | slot (1) | length (2, LE) | data | zero padding |
/// CRC-32 of everything before it (4, LE).
fn encode_record(slot: u8, data: &[u8], out: &mut [u8; RECORD_LEN]) -> Result<(), Error> {
    if data.len() > MAX_SECRET_LEN {
        return Err(Error::TooLong);
    }
    out.fill(0);
    out[..4].copy_from_slice(&MAGIC);
    out[4] = VERSION;
    out[5] = slot;
    out[6..8].copy_from_slice(&(data.len() as u16).to_le_bytes());
    out[HEADER_LEN..HEADER_LEN + data.len()].copy_from_slice(data);
    let crc = crc32(&out[..RECORD_LEN - CRC_LEN]);
    out[RECORD_LEN - CRC_LEN..].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Returns `Ok(None)` for an empty slot. A freshly erased encrypted sector decrypts to random
/// bytes, so "no magic" is treated as empty. A record with the right magic but wrong contents
/// is reported as [`Error::Corrupt`].
fn decode_record(slot: u8, record: &[u8]) -> Result<Option<&[u8]>, Error> {
    if record[..4] != MAGIC {
        return Ok(None);
    }
    if record[4] != VERSION || record[5] != slot {
        return Err(Error::Corrupt);
    }
    let len = usize::from(u16::from_le_bytes([record[6], record[7]]));
    if len > MAX_SECRET_LEN {
        return Err(Error::Corrupt);
    }
    let (body, crc_bytes) = record.split_at(RECORD_LEN - CRC_LEN);
    let stored_crc = u32::from_le_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
    if stored_crc != crc32(body) {
        return Err(Error::Corrupt);
    }
    Ok(Some(&record[HEADER_LEN..HEADER_LEN + len]))
}

/// CRC-32 (IEEE 802.3, the same as zlib). Bitwise: slow but tiny, and we only hash 124 bytes.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A [`TableBackend`] that keeps the table in the first sector of the `secrets` flash partition.
pub struct EncryptedFlash<'d> {
    flash: FlashStorage<'d>,
    /// Absolute flash offset of the partition (sector aligned).
    offset: u32,
    encrypted: bool,
}

/// The secret store for the real chip.
pub type EncryptedFlashStore<'d> = SlotStore<EncryptedFlash<'d>>;

impl<'d> EncryptedFlash<'d> {
    /// Finds the [`PARTITION_LABEL`] partition and checks that it is encrypted.
    ///
    /// Returns [`Error::NotEncrypted`] unless flash encryption is enabled **and** the partition
    /// has the `encrypted` flag, except when the `insecure-dev-store` feature is on.
    pub fn open(mut flash: FlashStorage<'d>) -> Result<Self, Error> {
        let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
        let table = partitions::read_partition_table(&mut flash, &mut buffer)?;
        let entry = table
            .iter()
            .find(|entry| entry.label_as_str() == PARTITION_LABEL)
            .ok_or(Error::PartitionNotFound)?;

        if (entry.len() as usize) < SECTOR_LEN {
            return Err(Error::PartitionTooSmall);
        }

        let flash_encryption_enabled = esp_storage::flash_encryption();
        let partition_flagged = entry.flags() & PART_FLAG_ENCRYPTED != 0;
        let encrypted = flash_encryption_enabled && partition_flagged;

        if encrypted {
            log::info!(
                "secret store: partition '{}' at 0x{:x} is encrypted",
                PARTITION_LABEL,
                entry.offset()
            );
        } else if cfg!(feature = "insecure-dev-store") {
            log::warn!(
                "secret store: INSECURE dev mode, secrets are stored in PLAIN TEXT \
                 (flash encryption enabled: {flash_encryption_enabled}, partition flagged: \
                 {partition_flagged})"
            );
        } else {
            return Err(Error::NotEncrypted {
                flash_encryption_enabled,
                partition_flagged,
            });
        }

        let offset = entry.offset();
        Ok(Self {
            flash,
            offset,
            encrypted,
        })
    }
}

impl<'d> EncryptedFlashStore<'d> {
    /// Opens the flash backend (see [`EncryptedFlash::open`]) and wraps it in a slot store.
    pub fn open(flash: FlashStorage<'d>) -> Result<Self, Error> {
        Ok(Self::new(EncryptedFlash::open(flash)?))
    }
}

impl TableBackend for EncryptedFlash<'_> {
    fn read_table(&mut self, table: &mut [u8; TABLE_LEN]) -> Result<(), Error> {
        if self.encrypted {
            self.flash.read_encrypted(self.offset, table)?;
        } else {
            self.flash.read_nor(self.offset, table)?;
        }
        Ok(())
    }

    fn write_table(&mut self, table: &[u8; TABLE_LEN]) -> Result<(), Error> {
        if self.encrypted {
            // Does its own read-modify-write, erase and encrypted write of the whole sector.
            self.flash.write_encrypted(self.offset, table)?;
        } else {
            // Plain NOR flash can only turn bits from 1 to 0, so the sector must be erased first.
            self.flash
                .erase(self.offset, self.offset + SECTOR_LEN as u32)?;
            self.flash.write_nor(self.offset, table)?;
        }
        Ok(())
    }
}
