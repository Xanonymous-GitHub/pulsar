//! The few ESP-IDF C calls that `esp-idf-svc` has no safe wrapper for.
//!
//! This is the only module in the crate allowed to use `unsafe`. Every function here is a thin,
//! documented wrapper that exposes a safe signature.

#![allow(
    unsafe_code,
    reason = "FFI calls into ESP-IDF; see the SAFETY comment on each"
)]

#[cfg(feature = "linux")]
use core::ffi::CStr;
use core::ffi::{c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use esp_idf_svc::sys;
use zeroize::{Zeroize, Zeroizing};

/// Whether flash encryption is turned on in the chip's eFuses.
#[must_use]
pub fn flash_encryption_enabled() -> bool {
    // SAFETY: the function only reads eFuse bits, takes no arguments and has no preconditions.
    unsafe { sys::esp_efuse_is_flash_encryption_enabled() }
}

/// Makes the station prefer a 5 GHz access point over a 2.4 GHz one when the 5 GHz signal is at
/// most `margin_db` weaker than the best 2.4 GHz signal. Call it after the station configuration
/// has been set and before connecting.
///
/// # Errors
///
/// Fails if the driver rejects the configuration.
pub fn prefer_5ghz(margin_db: u8) -> Result<(), sys::EspError> {
    // SAFETY: `wifi_config_t` is a plain C union of integers and byte arrays, for which the
    // all-zero bit pattern is valid.
    let mut config: sys::wifi_config_t = unsafe { core::mem::zeroed() };

    // SAFETY: `config` is valid for writes and outlives the calls. The station interface is
    // configured, and the driver copies the struct, so nothing keeps a pointer into it.
    // Reading `config.sta` is sound because `esp_wifi_get_config` filled the station variant.
    let result = sys::esp!(unsafe {
        sys::esp_wifi_get_config(sys::wifi_interface_t_WIFI_IF_STA, &raw mut config)
    })
    .and_then(|()| {
        // SAFETY: as above, `config.sta` is the variant that `esp_wifi_get_config` filled, and
        // `config` outlives the call.
        sys::esp!(unsafe {
            config.sta.threshold.rssi_5g_adjustment = margin_db;
            sys::esp_wifi_set_config(sys::wifi_interface_t_WIFI_IF_STA, &raw mut config)
        })
    });

    // The struct holds a copy of the Wi-Fi password: wipe it before it leaves scope.
    // SAFETY: the byte array is the station variant that was filled above.
    unsafe { config.sta.password.zeroize() };
    result
}

/// Allows Wi-Fi on both the 2.4 GHz and 5 GHz bands, with every protocol up to Wi-Fi 6 (802.11ax)
/// on each of them. Call it after the Wi-Fi driver has been configured and started.
///
/// # Errors
///
/// Fails if the driver rejects a setting.
pub fn enable_dual_band_wifi6() -> Result<(), sys::EspError> {
    /// 802.11b/g/n/ax on 2.4 GHz.
    const GHZ_2: u32 = sys::WIFI_PROTOCOL_11B
        | sys::WIFI_PROTOCOL_11G
        | sys::WIFI_PROTOCOL_11N
        | sys::WIFI_PROTOCOL_11AX;
    /// 802.11a/n/ac/ax on 5 GHz.
    const GHZ_5: u32 = sys::WIFI_PROTOCOL_11A
        | sys::WIFI_PROTOCOL_11N
        | sys::WIFI_PROTOCOL_11AC
        | sys::WIFI_PROTOCOL_11AX;

    let mut protocols = sys::wifi_protocols_t {
        ghz_2g: u16::try_from(GHZ_2)
            .map_err(|_| sys::EspError::from_infallible::<{ sys::ESP_ERR_INVALID_ARG }>())?,
        ghz_5g: u16::try_from(GHZ_5)
            .map_err(|_| sys::EspError::from_infallible::<{ sys::ESP_ERR_INVALID_ARG }>())?,
    };

    // SAFETY: both calls only read their arguments. `protocols` is a valid, initialised struct
    // that outlives the call, and the station interface exists once the driver has been started.
    sys::esp!(unsafe { sys::esp_wifi_set_band_mode(sys::wifi_band_mode_t_WIFI_BAND_MODE_AUTO) })?;
    sys::esp!(unsafe {
        sys::esp_wifi_set_protocols(sys::wifi_interface_t_WIFI_IF_STA, &raw mut protocols)
    })
}

/// A 32-bit number from the hardware random number generator. It is truly random only while the
/// radio (Wi-Fi or Bluetooth) is on, which is the case once either has started.
#[must_use]
pub fn random_u32() -> u32 {
    // SAFETY: the function takes no arguments, has no preconditions and only reads hardware.
    unsafe { sys::esp_random() }
}

/// The MAC address of the board: the factory-programmed one that the Wi-Fi station uses and the
/// router shows (unique per board).
///
/// # Errors
///
/// Fails if the eFuse block that holds the address cannot be read.
pub fn base_mac() -> Result<[u8; 6], sys::EspError> {
    // ESP-IDF's functions for MAC addresses write 6 or 8 bytes, depending on the kind of address
    // asked for and on the chip: the ESP32-C5 has an 802.15.4 radio, whose address is 8 bytes
    // long (an EUI-64). The buffer is 8 bytes whatever is asked, so a longer answer can never
    // write past it. (`esp_efuse_mac_get_default`, used here before, answers with the 8-byte
    // form on this chip: the 6-byte address with two more bytes put in the middle.)
    let mut buffer = [0_u8; 8];
    // SAFETY: the function writes at most 8 bytes to the pointer (6 for the station address
    // asked for here), which points to an 8-byte array that is valid for writes for the
    // duration of the call.
    sys::esp!(unsafe {
        sys::esp_read_mac(buffer.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_WIFI_STA)
    })?;
    let [mac @ .., _, _] = buffer;
    Ok(mac)
}

/// The free heap right now, in bytes.
#[must_use]
pub fn free_heap() -> u32 {
    // SAFETY: the function takes no arguments, has no preconditions and only reads heap counters.
    unsafe { sys::esp_get_free_heap_size() }
}

/// The smallest free heap since boot, in bytes: how close the firmware came to running out.
#[must_use]
pub fn min_free_heap() -> u32 {
    // SAFETY: the function takes no arguments, has no preconditions and only reads heap counters.
    unsafe { sys::esp_get_minimum_free_heap_size() }
}

/// The size of the largest block that can be allocated now, in bytes. TLS needs a few large
/// contiguous buffers, so this tells more than [`free_heap`] when the heap is fragmented.
#[must_use]
pub fn largest_free_block() -> usize {
    // SAFETY: the function only reads heap bookkeeping and has no preconditions.
    unsafe { sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT) }
}

/// The time since boot, from the high-resolution timer (not affected by the SNTP clock).
#[must_use]
pub fn uptime() -> Duration {
    // SAFETY: the function takes no arguments, has no preconditions and only reads the timer.
    let micros = unsafe { sys::esp_timer_get_time() };
    Duration::from_micros(u64::try_from(micros).unwrap_or_default())
}

/// Restarts the chip. Never returns.
pub fn restart() -> ! {
    // SAFETY: the function takes no arguments and has no preconditions. It runs the shutdown
    // handlers and resets the chip.
    unsafe { sys::esp_restart() }
}

/// Turns a NimBLE return code (0 is success) into a `Result`. The code itself is only logged,
/// because it is a NimBLE host error and not an `esp_err_t`.
fn ble_result(code: c_int) -> Result<(), sys::EspError> {
    if code == 0 {
        Ok(())
    } else {
        log::warn!("NimBLE call failed with code {code}");
        Err(sys::EspError::from_infallible::<{ sys::ESP_FAIL }>())
    }
}

/// Allows the 1M, 2M and Coded PHY for new Bluetooth connections. The link starts on 1M, as the
/// specification requires, and moves to 2M or Coded when the phone asks for it. Call it after the
/// Bluetooth host has started.
///
/// # Errors
///
/// Fails if the controller rejects the setting.
pub fn ble_allow_all_phys() -> Result<(), sys::EspError> {
    const MASK: u32 =
        sys::BLE_GAP_LE_PHY_1M_MASK | sys::BLE_GAP_LE_PHY_2M_MASK | sys::BLE_GAP_LE_PHY_CODED_MASK;
    let mask = u8::try_from(MASK)
        .map_err(|_| sys::EspError::from_infallible::<{ sys::ESP_ERR_INVALID_ARG }>())?;

    // SAFETY: the call only reads its two integer arguments.
    ble_result(unsafe { sys::ble_gap_set_prefered_default_le_phy(mask, mask) })
}

/// Lets new Bluetooth connections use the longest packets of Data Length Extension: 251 bytes of
/// payload, which take at most 2120 microseconds on the 1M PHY. Call it after the Bluetooth host
/// has started.
///
/// # Errors
///
/// Fails if the controller rejects the setting.
pub fn ble_use_long_packets() -> Result<(), sys::EspError> {
    /// The most payload bytes in one packet (Bluetooth Core 5.x, Vol 6, Part B, 4.5.10).
    const MAX_OCTETS: u16 = 251;
    /// The time they take on the 1M PHY: (251 + 14 bytes of overhead) * 8 bits at 1 Mbit/s.
    const MAX_TIME_US: u16 = 2120;

    // SAFETY: the call only reads its two integer arguments.
    ble_result(unsafe { sys::ble_hs_hci_util_write_sugg_def_data_len(MAX_OCTETS, MAX_TIME_US) })
}

/// The kinds of Bluetooth records that make up a bond. NimBLE keeps each of them as a C struct in
/// RAM; [`ble_bonds_export`] and [`ble_bonds_import`] move them in and out as raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BondObject {
    /// The keys we handed to the phone.
    OurSecurity,
    /// The keys the phone handed to us (long-term key, identity key).
    PeerSecurity,
    /// Which notifications the phone subscribed to.
    Cccd,
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "NimBLE's object type codes are small constants that fit a C int"
)]
mod nimble_type {
    use core::ffi::c_int;

    use esp_idf_svc::sys;

    pub const OUR_SEC: c_int = sys::BLE_STORE_OBJ_TYPE_OUR_SEC as c_int;
    pub const PEER_SEC: c_int = sys::BLE_STORE_OBJ_TYPE_PEER_SEC as c_int;
    pub const CCCD: c_int = sys::BLE_STORE_OBJ_TYPE_CCCD as c_int;
}

impl BondObject {
    /// Every kind of record that is saved.
    pub const ALL: [Self; 3] = [Self::OurSecurity, Self::PeerSecurity, Self::Cccd];

    /// A number that names this kind in the saved bond list. These are our own and never change,
    /// even if NimBLE renumbers its object types.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::OurSecurity => 1,
            Self::PeerSecurity => 2,
            Self::Cccd => 3,
        }
    }

    /// The kind with number `id`, as given by [`BondObject::id`].
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::OurSecurity),
            2 => Some(Self::PeerSecurity),
            3 => Some(Self::Cccd),
            _ => None,
        }
    }

    /// Size in bytes of this kind of record in this build of NimBLE. A saved record of another
    /// size comes from another layout and must not be loaded.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::OurSecurity | Self::PeerSecurity => size_of::<sys::ble_store_value_sec>(),
            Self::Cccd => size_of::<sys::ble_store_value_cccd>(),
        }
    }

    /// The `(offset, length)` of every field of this kind of record: all bytes that are not
    /// padding.
    fn fields(self) -> &'static [(usize, usize)] {
        /// One field of a C struct, as `(offset, length)`.
        macro_rules! field {
            ($record:ty, $name:ident, $type:ty) => {
                (core::mem::offset_of!($record, $name), size_of::<$type>())
            };
        }
        type Sec = sys::ble_store_value_sec;
        type Cccd = sys::ble_store_value_cccd;
        const SEC: [(usize, usize); 13] = [
            field!(Sec, peer_addr, sys::ble_addr_t),
            field!(Sec, bond_count, u16),
            field!(Sec, key_size, u8),
            field!(Sec, ediv, u16),
            field!(Sec, rand_num, u64),
            field!(Sec, ltk, [u8; 16]),
            field!(Sec, _bitfield_1, [u8; 1]),
            field!(Sec, irk, [u8; 16]),
            field!(Sec, _bitfield_2, [u8; 1]),
            field!(Sec, csrk, [u8; 16]),
            field!(Sec, _bitfield_3, [u8; 1]),
            field!(Sec, sign_counter, u32),
            field!(Sec, _bitfield_4, [u8; 1]),
        ];
        const CCCD: [(usize, usize); 4] = [
            field!(Cccd, peer_addr, sys::ble_addr_t),
            field!(Cccd, chr_val_handle, u16),
            field!(Cccd, flags, u16),
            field!(Cccd, _bitfield_1, [u8; 1]),
        ];
        match self {
            Self::OurSecurity | Self::PeerSecurity => &SEC,
            Self::Cccd => &CCCD,
        }
    }

    const fn nimble_type(self) -> c_int {
        match self {
            Self::OurSecurity => nimble_type::OUR_SEC,
            Self::PeerSecurity => nimble_type::PEER_SEC,
            Self::Cccd => nimble_type::CCCD,
        }
    }

    const fn from_nimble(code: c_int) -> Option<Self> {
        match code {
            nimble_type::OUR_SEC => Some(Self::OurSecurity),
            nimble_type::PEER_SEC => Some(Self::PeerSecurity),
            nimble_type::CCCD => Some(Self::Cccd),
            _ => None,
        }
    }
}

/// Set by the store hooks whenever a bond record changes, see [`ble_bonds_watch`].
static BONDS_CHANGED: AtomicBool = AtomicBool::new(false);
/// The store callbacks NimBLE had before [`ble_bonds_watch`] replaced them.
static CHAINED_WRITE: OnceLock<sys::ble_store_write_fn> = OnceLock::new();
static CHAINED_DELETE: OnceLock<sys::ble_store_delete_fn> = OnceLock::new();

/// Calls `each` with the raw bytes of every stored bond record. The bytes are secret key
/// material: do not log them, and wipe copies when you are done.
///
/// # Errors
///
/// Fails if NimBLE cannot read its store.
pub fn ble_bonds_export(mut each: impl FnMut(BondObject, &[u8])) -> Result<(), sys::EspError> {
    /// Called by NimBLE once for every stored record of the iterated kind.
    unsafe extern "C" fn visit(
        code: c_int,
        value: *mut sys::ble_store_value,
        cookie: *mut c_void,
    ) -> c_int {
        let Some(object) = BondObject::from_nimble(code) else {
            return 0;
        };
        // SAFETY: `cookie` is the address of the `&mut dyn FnMut` below, which lives until
        // `ble_store_iterate` returns, and nothing else uses it meanwhile.
        let each = unsafe { &mut *cookie.cast::<&mut dyn FnMut(BondObject, &[u8])>() };
        // The C structs have padding bytes that NimBLE never writes, so they may be
        // uninitialised. Only the fields are copied, into a zeroed buffer: the padding of the
        // copy is zero and no uninitialised byte is read.
        let mut bytes = Zeroizing::new(vec![0_u8; object.size()]);
        for &(offset, length) in object.fields() {
            // SAFETY: NimBLE passes a valid value of the kind `code` names, which is
            // `object.size()` bytes long, and `fields` lists spans inside it. A field is made of
            // integers and was written by NimBLE. The buffers do not overlap.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    value.cast_const().cast::<u8>().add(offset),
                    bytes.as_mut_ptr().add(offset),
                    length,
                );
            }
        }
        each(object, &bytes);
        0
    }

    let mut each: &mut dyn FnMut(BondObject, &[u8]) = &mut each;
    for object in BondObject::ALL {
        // SAFETY: `visit` matches the callback type, and `cookie` points to `each`, which
        // outlives the call. Iterating only reads the store.
        ble_result(unsafe {
            sys::ble_store_iterate(
                object.nimble_type(),
                Some(visit),
                (&raw mut each).cast::<c_void>(),
            )
        })?;
    }
    Ok(())
}

/// Puts one saved bond record back into NimBLE's store. Call it after the Bluetooth host has
/// started and before [`ble_bonds_watch`], so loading does not count as a change.
///
/// # Errors
///
/// Fails if `bytes` is not exactly [`BondObject::size`] bytes long, or NimBLE refuses the record.
pub fn ble_bonds_import(object: BondObject, bytes: &[u8]) -> Result<(), sys::EspError> {
    if bytes.len() != object.size() {
        return Err(sys::EspError::from_infallible::<
            { sys::ESP_ERR_INVALID_SIZE },
        >());
    }

    let mut value = sys::ble_store_value::default();
    // SAFETY: `value` is a union that is at least as big as every record (checked above), and
    // every member is made of integers only, so any bytes are a valid value. The two buffers
    // cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), (&raw mut value).cast::<u8>(), bytes.len());
    }
    let result = match object {
        // SAFETY: `value` is initialised and outlives the call, which copies what it needs. The
        // bytes are a `ble_store_value_sec` (see `BondObject::size`). This variant of the write
        // also gives the phone's identity key to the controller, so that it recognises the
        // phone's private addresses: the plain write only fills NimBLE's store, and NimBLE loaded
        // its own copy of the keys before these records came back.
        BondObject::PeerSecurity => {
            ble_result(unsafe { sys::ble_store_write_peer_sec(&raw const value.sec) })
        }
        // SAFETY: `value` is initialised and outlives the call, which copies what it needs.
        BondObject::OurSecurity | BondObject::Cccd => {
            ble_result(unsafe { sys::ble_store_write(object.nimble_type(), &raw const value) })
        }
    };

    // SAFETY: the union holds secret keys: overwrite it with zeros before it goes out of scope.
    // Volatile writes keep the compiler from removing this as a dead store.
    unsafe {
        core::ptr::write_volatile(&raw mut value, sys::ble_store_value::default());
    }
    result
}

/// Starts to track changes to the bond records: after this, [`ble_bonds_take_changed`] tells
/// whether a record was written or deleted since it was last asked. Call it once, after the
/// Bluetooth host has started.
pub fn ble_bonds_watch() {
    /// NimBLE's "not supported" code, returned if there is no callback to hand over to.
    const NOT_SUPPORTED: c_int = 8;

    unsafe extern "C" fn on_write(code: c_int, value: *const sys::ble_store_value) -> c_int {
        let Some(Some(chained)) = CHAINED_WRITE.get().copied() else {
            return NOT_SUPPORTED;
        };
        // SAFETY: this hands NimBLE's own arguments, unchanged, to the callback NimBLE had
        // registered before ours.
        let code_result = unsafe { chained(code, value) };
        if code_result == 0 && BondObject::from_nimble(code).is_some() {
            BONDS_CHANGED.store(true, Ordering::Release);
        }
        code_result
    }

    unsafe extern "C" fn on_delete(code: c_int, key: *const sys::ble_store_key) -> c_int {
        let Some(Some(chained)) = CHAINED_DELETE.get().copied() else {
            return NOT_SUPPORTED;
        };
        // SAFETY: as in `on_write`.
        let code_result = unsafe { chained(code, key) };
        if code_result == 0 && BondObject::from_nimble(code).is_some() {
            BONDS_CHANGED.store(true, Ordering::Release);
        }
        code_result
    }

    // SAFETY: the host configuration is only changed here, once, before any pairing can start,
    // and NimBLE only reads the two callbacks afterwards. The old callbacks are kept, so storing
    // and reading records work exactly as before.
    unsafe {
        let config = &raw mut sys::ble_hs_cfg;
        if CHAINED_WRITE.set((*config).store_write_cb).is_ok()
            && CHAINED_DELETE.set((*config).store_delete_cb).is_ok()
        {
            (*config).store_write_cb = Some(on_write);
            (*config).store_delete_cb = Some(on_delete);
        }
    }
}

/// Whether a bond record changed since the last call. See [`ble_bonds_watch`].
#[must_use]
pub fn ble_bonds_take_changed() -> bool {
    BONDS_CHANGED.swap(false, Ordering::AcqRel)
}

/// Hands out the board's PSRAM, all of it, as one block of bytes. It can be taken once: later
/// calls return `None`, as does a board without PSRAM.
///
/// This needs `CONFIG_SPIRAM_USE_MEMMAP`: the PSRAM is mapped into the address space but given
/// to no allocator, so nothing else in the firmware uses it.
#[cfg(feature = "linux")]
#[must_use]
pub fn psram_take() -> Option<&'static mut [u8]> {
    static TAKEN: AtomicBool = AtomicBool::new(false);

    // SAFETY: both functions take no arguments, have no preconditions and only read the state
    // that the PSRAM driver set up before `main` ran.
    let size = unsafe {
        if !sys::esp_psram_is_initialized() {
            return None;
        }
        sys::esp_psram_get_size()
    };
    if size == 0 {
        return None;
    }

    // Where the first byte of the PSRAM is mapped, and whether the mapping goes on without a
    // gap to its last byte.
    let mut start: *mut c_void = core::ptr::null_mut();
    let mut last_offset: sys::esp_paddr_t = 0;
    let mut last_target: sys::mmu_target_t = 0;
    // SAFETY: the calls only read the MMU tables. Each pointer argument points to a local of
    // the type the function writes, valid for the duration of the call. The address passed to
    // the second call is not dereferenced by it.
    let contiguous = unsafe {
        sys::esp!(sys::esp_mmu_paddr_to_vaddr(
            0,
            sys::mmu_target_t_MMU_TARGET_PSRAM0,
            sys::mmu_vaddr_t_MMU_VADDR_DATA,
            &raw mut start,
        ))
        .is_ok()
            && sys::esp!(sys::esp_mmu_vaddr_to_paddr(
                start.cast::<u8>().wrapping_add(size - 1).cast(),
                &raw mut last_offset,
                &raw mut last_target,
            ))
            .is_ok()
    };
    let whole = contiguous
        && last_target == sys::mmu_target_t_MMU_TARGET_PSRAM0
        && usize::try_from(last_offset) == Ok(size - 1);
    if start.is_null() || !whole || TAKEN.swap(true, Ordering::AcqRel) {
        return None;
    }

    // SAFETY: `start` is the mapped address of PSRAM offset 0, and the check above showed that
    // the `size` bytes from there are mapped to the PSRAM in order. The mapping is made at boot
    // and never removed. With `CONFIG_SPIRAM_USE_MEMMAP` (enforced in `src/lib.rs`) ESP-IDF gives
    // this memory to no allocator and puts nothing of its own there, and `TAKEN` makes this the
    // only reference ever created to it. Any bit pattern is a valid `u8`.
    Some(unsafe { core::slice::from_raw_parts_mut(start.cast::<u8>(), size) })
}

/// A data partition of the flash, found by its name in the partition table.
#[cfg(feature = "linux")]
#[derive(Clone, Copy, Debug)]
pub struct Partition(&'static sys::esp_partition_t);

#[cfg(feature = "linux")]
impl Partition {
    /// Looks for the data partition called `label`.
    #[must_use]
    pub fn find(label: &CStr) -> Option<Self> {
        // SAFETY: `label` is a valid C string for the duration of the call, which only reads the
        // partition table. The result is null or points to an entry that ESP-IDF keeps, unchanged,
        // for as long as the program runs.
        unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                label.as_ptr(),
            )
            .as_ref()
        }
        .map(Self)
    }

    /// The size of the partition in bytes.
    #[must_use]
    pub fn size(self) -> usize {
        usize::try_from(self.0.size).unwrap_or(usize::MAX)
    }

    /// Reads `buffer.len()` bytes that start `offset` bytes into the partition. The buffer must
    /// be in internal RAM (not PSRAM).
    ///
    /// # Errors
    ///
    /// Fails if the range is not inside the partition or the flash cannot be read.
    pub fn read(self, offset: usize, buffer: &mut [u8]) -> Result<(), sys::EspError> {
        // SAFETY: the partition entry is valid (see `find`), and the function writes exactly
        // `buffer.len()` bytes to `buffer`, which is valid for writes of that length. It checks
        // the range against the partition itself.
        sys::esp!(unsafe {
            sys::esp_partition_read(self.0, offset, buffer.as_mut_ptr().cast(), buffer.len())
        })
    }
}

/// How long [`console_write`] waits for room in the USB transmit buffer, in ticks (milliseconds).
/// Without a host that reads, the data is dropped after this.
#[cfg(feature = "linux")]
const CONSOLE_WRITE_WAIT_TICKS: u32 = 20;

/// Takes over the USB serial port for [`console_read`] and [`console_write`]: installs the
/// interrupt-driven driver and makes the standard output (the log) use it too, so both can share
/// the port. Call it once.
///
/// # Errors
///
/// Fails if the driver cannot be installed.
#[cfg(feature = "linux")]
pub fn console_claim() -> Result<(), sys::EspError> {
    let mut config = sys::usb_serial_jtag_driver_config_t {
        tx_buffer_size: 2048,
        rx_buffer_size: 256,
    };
    // SAFETY: `config` is a valid, initialised struct that outlives the call, which copies what
    // it needs. Installing is checked first, because a second install would fail. The second
    // call only switches which functions the standard streams use.
    unsafe {
        if !sys::usb_serial_jtag_is_driver_installed() {
            sys::esp!(sys::usb_serial_jtag_driver_install(&raw mut config))?;
        }
        sys::usb_serial_jtag_vfs_use_driver();
    }
    Ok(())
}

/// Reads the bytes the host has sent over the USB serial port into `buffer`, without waiting.
/// Returns how many there were. Call [`console_claim`] first.
#[cfg(feature = "linux")]
#[must_use]
pub fn console_read(buffer: &mut [u8]) -> usize {
    let Ok(length) = u32::try_from(buffer.len()) else {
        return 0;
    };
    if length == 0 {
        return 0;
    }
    // SAFETY: the driver is installed (see `console_claim`). The function writes at most
    // `length` bytes to `buffer`, which is valid for writes of that length, and with a wait of
    // zero ticks it returns at once.
    let read = unsafe { sys::usb_serial_jtag_read_bytes(buffer.as_mut_ptr().cast(), length, 0) };
    usize::try_from(read).unwrap_or(0)
}

/// Sends `bytes` to the host over the USB serial port, exactly as they are. If the host does
/// not read, what does not fit in the buffer is dropped after a short wait. Call
/// [`console_claim`] first.
#[cfg(feature = "linux")]
pub fn console_write(bytes: &[u8]) {
    // SAFETY: the driver is installed (see `console_claim`). The function reads `bytes.len()`
    // bytes from `bytes`, which is valid for reads of that length, and copies them.
    unsafe {
        sys::usb_serial_jtag_write_bytes(
            bytes.as_ptr().cast(),
            bytes.len(),
            CONSOLE_WRITE_WAIT_TICKS,
        );
    }
}

/// Makes ESP-IDF's own components (Wi-Fi, the network stack, ...) log warnings and errors only.
#[cfg(feature = "linux")]
pub fn quiet_system_log() {
    // SAFETY: the tag is a valid C string (the wildcard for every component) for the duration
    // of the call, which only changes log filters.
    unsafe { sys::esp_log_level_set(c"*".as_ptr(), sys::esp_log_level_t_ESP_LOG_WARN) };
}
