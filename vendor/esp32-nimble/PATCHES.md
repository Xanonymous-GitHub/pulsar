# Local patches to esp32-nimble 0.13.0

This directory is `esp32-nimble` 0.13.0 as published on crates.io, with the changes below. They are
marked `PATCH(pulsar)` in the source. Drop this directory (and `[patch.crates-io]` in the root
`Cargo.toml`) when upstream fixes both problems.

1. **`src/server/ble_server.rs`, `BLE_GAP_EVENT_PASSKEY_ACTION`:** an action the crate does not
   handle (for example `BLE_SM_IOACT_OOB_SC`, which NimBLE picks when the *peer* claims to have
   out-of-band data) hit `todo!()`. That panic inside a C callback aborts the firmware, and any
   central in range can trigger it. The patch logs and disconnects the peer instead.
2. **`src/server/ble_characteristic.rs`, `subscribed_list`:** `subscribe()` runs on the NimBLE host
   task and changed the list without a lock, while `notify()` walked it on the caller's task. The
   list is now behind the crate's `Mutex`, and `notify()` sends from a copy so the lock is not held
   while sending.

3. **`src/server/ble_ext_advertising.rs`:** new `BLEExtAdvertisement::flags()`, because the
   extended-advertising API had no way to add the Flags field. Without it, phones' Bluetooth
   settings pages do not list the device.

4. **`src/server/ble_characteristic.rs`, `send_value`:** `ble_att_mtu(conn) - 3` underflowed when
   the phone disconnected between `notify()` copying the subscriber list and sending (the MTU of
   a missing connection is 0), which panics in builds with overflow checks. It now saturates, so
   the existing `mtu == 0` check returns an error instead.

Removed from the copy: examples, CI files, the toolchain file and the dev-dependencies.
