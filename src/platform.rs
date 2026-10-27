//! The few ESP-IDF C calls that `esp-idf-svc` has no safe wrapper for.
//!
//! This is the only module in the crate allowed to use `unsafe`. Every function here is a thin,
//! documented wrapper that exposes a safe signature.

#![allow(
    unsafe_code,
    reason = "FFI calls into ESP-IDF; see the SAFETY comment on each"
)]

use esp_idf_svc::sys;
use zeroize::Zeroize;

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
        config.sta.threshold.rssi_5g_adjustment = margin_db;
        sys::esp!(unsafe {
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
