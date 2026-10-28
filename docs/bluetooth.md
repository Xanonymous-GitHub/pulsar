# Bluetooth LE

Pulsar speaks Bluetooth Low Energy through the ESP-IDF **NimBLE** host and the `esp32-nimble` crate. The code is in `src/bluetooth/`; the shared status type is in `src/status.rs`; the icon is drawn by `src/ui/status_bar.rs`.

## What the chip can do

The ESP32-C5 has a **BLE-only** radio (no Bluetooth Classic) on 2.4 GHz, with a U.FL antenna connector. Seeed lists it as "Bluetooth 5 (LE)" with Bluetooth mesh. ESP-IDF v6.1 (`soc_caps.h` for the C5) also reports support for device privacy, LE Power Control, periodic-advertising enhancements, Constant Tone Extension, Connection Subrating and PAwR. Channel Sounding (Bluetooth 6.0) has no support flag for the C5.

| Feature | Version | State here | Why |
|---|---|---|---|
| LE Secure Connections | 4.2 | **On, and the only pairing mode** | Legacy pairing can be broken passively. `BT_NIMBLE_SM_LEGACY=n`, `BT_NIMBLE_SM_SC_ONLY=1`. |
| Data Length Extension (251 B) | 4.2 | **On** | Up to 251 bytes per packet. `platform::ble_use_long_packets`. |
| MTU 512 | 4.0+ | **On** | `config::BLE_PREFERRED_MTU`. |
| Link Layer Privacy | 4.2 | **On for peers** | The phone's resolvable private address is resolved with the bonded IRK. The board itself advertises its public address, so it has one stable identity. |
| LE 2M PHY | 5.0 | **On** | Allowed, with 1M and Coded, by `platform::ble_allow_all_phys`; the controller and the phone choose. |
| LE Coded PHY | 5.0 | **On** | Advertising set 1 and extended scanning. Coded PHY at 125 kbit/s (S=8) trades speed for range; the gain depends on the phone and the surroundings. |
| Extended advertising | 5.0 | **On, two sets** | Set 0 is legacy (every phone sees it), set 1 is extended on Coded PHY. |
| Channel Selection Algorithm #2 | 5.0 | Automatic | Done by the controller. |
| Observer, Broadcaster, Peripheral at once | 4.0+ | **On** | The Central role is not used yet. |
| Periodic advertising, PAST | 5.0, 5.1 | Off | No user, and it costs RAM. `BT_NIMBLE_ENABLE_PERIODIC_ADV`. |
| Direction finding (AoA/AoD) | 5.1 | Off | Needs an antenna array. The board has one antenna. `BT_NIMBLE_AOA_AOD`. |
| GATT caching | 5.1 | Off | Client-side feature. `BT_NIMBLE_GATT_CACHING`. |
| EATT | 5.2 | Off | Needs host support that is marked experimental in NimBLE's Kconfig, and phones rarely use it. `BT_NIMBLE_EATT_CHAN_NUM`. |
| LE Power Control | 5.2 | Off | Host support is marked experimental in NimBLE's Kconfig. `BT_NIMBLE_BLE_POWER_CONTROL`. |
| Connection Subrating | 5.3 | Off | Host support is marked experimental in NimBLE's Kconfig. `BT_NIMBLE_SUBRATE`. |
| PAwR, Encrypted Advertising Data | 5.4 | Off | No consumer, and host support is marked experimental in NimBLE's Kconfig. |
| Bluetooth Mesh 1.1 | 5.x | Off | Preview API and a separate stack model. |
| Channel Sounding, Decision-Based Advertising Filtering | 6.0 | Not available | No support on the C5 or in the current hosts. |

To turn a feature on, change its `CONFIG_` line in `sdkconfig.defaults`, then do a clean build of ESP-IDF (delete `target/*/build/esp-idf-sys-*`).

## The status icon

The icon sits at the right of the second line of the status bar. Two rules read it:

- **Filled means a phone is connected. Hollow means nobody is.**
- **Steady means paired (bonded and encrypted). Blinking means not paired yet.**

| What is happening | Icon |
|---|---|
| Bluetooth is off, or not advertising yet (see below) | none |
| Bluetooth failed to start | hollow frame with a cross |
| Discoverable, no bond stored | hollow, blinking |
| Paired phone known, not connected | **hollow, steady** |
| Connected, pairing not done | filled, blinking (the passkey is shown next to it while pairing) |
| Paired and connected | **filled, steady** |

## Pairing and bonds

1. Open nRF Connect (or any BLE app) on the phone and connect to `pulsar`.
2. Read the `wifi` or `weather` characteristic of the Pulsar service. Both need an authenticated, encrypted link, so the phone starts pairing.
3. A six-digit passkey appears on the OLED, next to the icon. Type it on the phone. The icon turns steady.

The board has a display and no keyboard, so it uses the `DisplayOnly` IO capability. With Secure Connections that gives Security Mode 1 Level 4 (authenticated, 128-bit key) *when the phone has a keyboard*. A phone with no input (`NoInputNoOutput`) can still bond with "Just Works": that link is encrypted but not authenticated. It is not counted as paired (the icon blinks) and it cannot read the Pulsar values, which need an authenticated link. The passkey is random per pairing and is never written to the log.

**Where the keys are kept.** NimBLE can store bonds itself (`BT_NIMBLE_NVS_PERSIST`), but it writes them in **plain text** to the `nvs` partition, which flash encryption does not cover. Pulsar leaves that option off. `src/bluetooth/bonds.rs` keeps the bond records in the encrypted secret store (key `ble-bonds`, see `docs/secure-wifi-secrets.md`). Without the secret store, bonds live in RAM only and a phone has to pair again after every reboot.

The stored blob starts with a magic number, a format version and the size of every NimBLE C struct it holds. If an ESP-IDF update changes one of those sizes, the blob is dropped and phones pair again. That is on purpose: reading old bytes into a new layout would give the stack garbage keys.

## Wi-Fi and Bluetooth together

Wi-Fi and BLE share the 2.4 GHz radio, and the chip arbitrates between them (software coexistence, on by default). Expect lower Wi-Fi throughput on 2.4 GHz while BLE is busy. The board prefers 5 GHz for Wi-Fi; Bluetooth does not share that band, but the measurement below shows BLE can still slow Wi-Fi's scan.

**Measured on this board:** Wi-Fi's complete scan of both bands takes about 13 seconds without Bluetooth, and about 24 seconds as soon as *any* advertising set is on. The interval (100 ms and 1000 ms gave the same result) and the PHY make no difference, and a Bluetooth stack that is up but not advertising costs nothing. So `bluetooth::start` brings the stack up at once, and its thread starts advertising when Wi-Fi's first connection attempt is over (`config::BLE_WIFI_WAIT` is the longest it waits). In practice advertising begins about 16 seconds after power-on. A Wi-Fi reconnect later (the router restarted) takes the longer scan, which is why `config::WIFI_CONNECT_TIMEOUT` is 45 seconds and not the 15 seconds `esp-idf-svc` hard-codes.

## Known limits

- **The Coded PHY set needs a phone that supports it.** Many do not (check your phone model; Android support varies by device). Use the normal set (the one every phone sees) for everyday pairing.
- **One phone per advertising set.** A connectable set ends when a phone connects to it. The board restarts the sets when a phone disconnects.
- **Pair one phone at a time.** The stack does not say which connection a passkey is for, so the passkey on the screen is cleared when any pairing ends.
- **Notifications are one ATT packet.** A value longer than the negotiated MTU minus 3 bytes is cut in a notification. A read returns all of it.
- **Bond order.** NimBLE renumbers `bond_count` (its age counter) when a record is stored again, so a bond restored after a reboot differs from the saved one in that field (a 16-bit number). Everything else, the keys included, is restored unchanged.
