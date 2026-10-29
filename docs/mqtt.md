# MQTT

The board speaks MQTT through the ESP-IDF `esp-mqtt` client (the managed component `espressif/mqtt`, see `Cargo.toml`). The code is in three layers:

| Module | Role |
|---|---|
| `src/mqtt/` | A general MQTT client: connect, publish, subscribe with handlers, reconnect. No application logic. |
| `src/telemetry.rs` | What the board does with it: availability, a JSON state, commands. |
| `src/home_assistant.rs` | The discovery message that makes Home Assistant list the board as a device. |

## Configure it

The broker is set at build time, like the Wi-Fi network, and kept in the encrypted secret store (key `mqtt`):

```sh
export PULSAR_MQTT_URL='mqtts://broker.example.org:8883'
export PULSAR_MQTT_USERNAME='pulsar'
read -rs PULSAR_MQTT_PASSWORD && export PULSAR_MQTT_PASSWORD
cargo run --features insecure-dev-store
```

- `PULSAR_MQTT_URL` is required to turn MQTT on. Username and password are optional (a password needs a username), and never go inside the URL.
- Later builds can leave all three unset: the stored settings are used. With nothing stored, MQTT is off and the log says so.
- Setting a username or password without the URL is refused.
- `PULSAR_MQTT_URL=off` erases the stored settings and turns MQTT off.

## What is supported

| Feature | State |
|---|---|
| MQTT 5.0 | Yes. `config::MQTT_VERSION` is `MqttVersion::V5` (the default). Session expiry, will delay, receive maximum and maximum packet size are sent in CONNECT. |
| MQTT 3.1.1 | Yes. Set `config::MQTT_VERSION` to `MqttVersion::V3_1_1` for a broker without version 5. |
| `mqtt://` (TCP) | Yes. |
| `mqtts://` (TLS 1.3, 1.2 fallback) | Yes. The server certificate, host name and dates are checked against the built-in Mozilla roots. The board waits for the SNTP clock first. |
| `ws://`, `wss://` (WebSocket) | Yes. Put the broker's path in the URL, for example `wss://broker.example.org/mqtt`. |
| Username and password | Yes. Redacted in `Debug`, wiped on drop, stored encrypted. |
| QoS 0, 1, 2; retained messages | Yes. |
| Last Will | Yes, with a delay on MQTT 5 so that a quick reconnect cancels it. |
| Wildcards (`+`, `#`), `$SYS` rules | Yes, one handler per subscription. |
| Shared subscriptions (`$share/g/...`) | The filter is accepted (MQTT 5 brokers). |
| Reconnect, resubscribe | Yes. The MQTT task reconnects every 10 s; subscriptions are sent again unless the broker kept the session. |
| Periodic tasks | Yes, `Client::every`, on the client's own thread (no extra thread for readings). |
| Messages larger than the receive buffer | Yes, reassembled up to 6 KiB (`MQTT_MAX_INCOMING_BYTES`); longer ones are dropped with a warning. With MQTT 5 the broker is told the limit and does not send them. |
| Offline queue | Yes, a 16 KiB outbox. A full outbox is an error (`mqtt::Error::OutboxFull`). |
| Client certificates (mutual TLS), custom CA | No. Not wired yet. |
| MQTT 5 properties on single messages (user properties, response topic, expiry), reason codes, retain flag and QoS of incoming messages | No. `esp-idf-svc` does not expose them. |
| Seeing the broker's refusal of a subscription or login | No, only in the log. |

## Topics

`<id>` is the last three bytes of the MAC address in hex (for example `a1b2c3`). The client id is `pulsar-<id>`.

| Topic | Direction | Content |
|---|---|---|
| `pulsar/<id>/availability` | board → broker | `online` (retained) when connected. `offline` (retained) is the Last Will, and is also sent on purpose before a restart. |
| `pulsar/<id>/state` | board → broker | Retained JSON, sent on connect and every minute (see below). |
| `pulsar/<id>/cmd/ping` | broker → board | Any text. The board publishes it on `pulsar/<id>/pong`. |
| `pulsar/<id>/cmd/notice` | broker → board | Text for the screen, shown for 30 s instead of the weather. ASCII only (other characters become `?`), at most 100 characters. |
| `pulsar/<id>/cmd/restart` | broker → board | Any message restarts the board, except in the first 10 s after the connection came up (see Security notes). |
| `homeassistant/device/pulsar-<id>/config` | board → broker | Retained Home Assistant discovery message. |

The state message:

```json
{"ssid":"my-network","rssi":-54,"band":"5 GHz","ip":"192.168.1.42","uptime_s":3600,"free_heap":123456,"min_free_heap":98765,"weather":"Taipei 27C Overcast | Today 24-29C, rain 40% | Wind 12 km/h"}
```

## Home Assistant

With the MQTT integration pointed at the same broker, the board appears as the device **Pulsar a1b2c3** (Home Assistant 2024.11 or newer, which reads the combined "device" discovery format). It has:

- Sensors: signal strength, Wi-Fi band, Wi-Fi network, IP address, uptime, free memory (all under Diagnostic) and the weather text.
- A **Restart** button.
- A **Notice** text box that shows what you type on the OLED.

The discovery message is retained, so Home Assistant finds the board after its own restart without help from the board. The device turns unavailable when `availability` says `offline`.

## Try it with mosquitto

```sh
brew install mosquitto

# Watch everything the board sends (add -u/-P and -p/--cafile for your broker):
mosquitto_sub -h broker.example.org -t 'pulsar/#' -t 'homeassistant/device/#' -v

mosquitto_pub -h broker.example.org -t pulsar/a1b2c3/cmd/ping   -m hello      # -> pulsar/a1b2c3/pong hello
mosquitto_pub -h broker.example.org -t pulsar/a1b2c3/cmd/notice -m 'Hi there' # shows on the OLED for 30 s
mosquitto_pub -h broker.example.org -t pulsar/a1b2c3/cmd/restart -m now
```

To check long messages, publish more than 2 KiB (and less than 6 KiB) to a command topic, for example `ping`, and look for the same text on `pong`. More than 6 KiB is dropped, with a warning in the log.

## Memory

RAM is the tight resource on this board: with Wi-Fi, Bluetooth and the display running, about 80 KB of heap are free, and a thread costs its whole stack from it. The MQTT client costs about 20 KB (the ESP-IDF MQTT task with 6 KiB of stack, 10 KiB for TLS; the handler thread with 5 KiB; the buffers). So:

- Telemetry has no thread of its own after start-up: its set-up thread ends, and the state goes out from the client's thread (`Client::every`).
- `sdkconfig.defaults` turns on mbedTLS **dynamic buffers** and caps the **TLS record size at 8 KiB**. Without them a TLS session keeps about 20 KB resident and needs a free 16 KiB block for every record; with MQTT over TLS next to the weather requests that ran the heap out (`alloc(16725 bytes) failed`, handshake error `-0x008D`). The price: a server that sends a TLS record longer than 8 KiB breaks the connection. That is why incoming MQTT messages are limited to 6 KiB.
- With MQTT running the heap stays above about 20 KB at its lowest (measured while the weather's TLS handshake ran). If you add more features, watch the line `MQTT online, free heap ... (lowest ..., largest block ...)` in the log.

## Security notes

- **Use TLS** (`mqtts://` or `wss://`) for anything beyond a test: with `mqtt://` the password and every command cross the network in clear text.
- **Commands have no password of their own.** Anyone who may publish to `pulsar/<id>/cmd/#` can restart the board or write on its screen. Give the board's login read access to its command topics only, and give only people you trust write access to them.
- The board's login should be allowed to publish only under `pulsar/<id>/` and `homeassistant/device/pulsar-<id>/`, and to subscribe only to `pulsar/<id>/cmd/+`.
- A command published *retained* is replayed by the broker after every subscribe. The board ignores a `restart` that comes within 10 s of the connection coming up (`RESTART_GRACE`), whatever the access rules say, and also tries to clear it on the broker, so a mistake cannot lock it in a restart loop. A real `restart` inside that window has to be sent again. Empty messages are never commands.
- Payloads and credentials are never logged. The ESP-IDF client keeps the password in RAM while it runs, and a build made with the environment variables contains it in the firmware image (as with the Wi-Fi password).
