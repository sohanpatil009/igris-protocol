# igris-protocol

Shared LAN device-ecosystem protocol core for **IGRIS**. This crate is the
single source of truth for every wire protocol in the mesh — discovery,
pairing/trust, clipboard sync, notification sync, calls, SMS, LAN call
audio, and file transfer — plus the UniFFI surface that lets mobile
companions drive the exact same code.

Both endpoints consume it:

- **Desktop** ([`igris-ecosystem`](../igris-ecosystem)) links it as a path
  dependency and re-exports it (`pub use igris_protocol::{eco, fastswap}`),
  so the desktop binary runs this code in-process.
- **Android** ([`igris-andriod`](../igris-andriod)) cross-compiles it with
  `cargo ndk` to `libigris_protocol.so` (arm64-v8a, armeabi-v7a, x86_64) and
  drives it over UniFFI-generated Kotlin bindings via JNA.

Because both ends execute the same crate, the wire protocols cannot drift
apart between platforms.

---

## Layout

```
src/
├── lib.rs            # Module wiring + uniffi::setup_scaffolding!()
├── eco/              # Ecosystem engine (clipboard, notifications, calls, SMS, audio)
│   ├── manager.rs        #   EcoManager lifecycle (init → start → shutdown)
│   ├── discovery.rs      #   Axum server (53327) + multi-interface subnet scan + TLS proxy (53328)
│   ├── protocol.rs       #   EcoMessage envelope + every payload type (versioned)
│   ├── pairing.rs        #   Trust store, one-click link handshake, OTP machinery
│   ├── clipboard.rs      #   ClipboardManager — 1 s poll, SHA-256 diff, loop-proof apply
│   ├── notification.rs   #   NotificationManager — keyed dedup, reply, dismiss, icon hash-skip
│   ├── call.rs           #   CallManager + CallLogManager (enterprise calls)
│   ├── sms.rs            #   SmsManager (enterprise SMS bridge)
│   ├── audio.rs          #   Call-audio routing decisions (parked-route state machine)
│   ├── audio_net.rs      #   LAN audio channel — raw duplex PCM over one TCP socket (53329)
│   ├── sync.rs           #   Fan-out to trusted peers
│   ├── transport.rs      #   HTTPS client for peer pushes
│   ├── storage.rs        #   JSON persistence (trust, devices, history)
│   ├── crypto.rs         #   Self-signed certs (rcgen) + TLS plumbing
│   ├── device.rs         #   Device model + capabilities
│   ├── events.rs         #   Pub/sub EventBus (ClipboardChanged, DeviceDiscovered, …)
│   ├── permissions.rs / config.rs / constants.rs / errors.rs
├── fastswap/         # Igris-native file transfer
│   ├── network/
│   │   ├── server.rs     #   Axum receiver (53317): info/register/prepare/confirm/upload
│   │   ├── client.rs     #   Sender (prepare → confirm → chunked parallel upload)
│   │   └── discovery.rs  #   HTTPS subnet scan for FastSwap peers
│   ├── models/           #   Device, FileInfo, TransferProgress (speed/ETA), sessions
│   └── tls.rs            #   Self-signed cert generation (rcgen + ring)
├── platform/         # Platform seams — traits only, implementations injected
│   ├── ecosystem/        #   PlatformClipboard, PlatformNotification, PlatformContacts, hooks
│   └── …
└── ffi/              # UniFFI surface for Kotlin (EcosystemNode + provider/listener traits)
```

Unlike a full application, this crate contains **no `#[cfg]` platform
factories**. The host injects behavior:

- **Desktop**: `igrisecosystem::platform::ecosystem::{create_platform_clipboard, notifications::create_platform_notification}` plus audio/call hooks registered at startup.
- **Android**: Kotlin implements `ClipboardProvider`, `NotificationProvider`,
  and `ContactProvider`; events flow back through `NodeEventListener`.

---

## Wire protocols

### Ecosystem REST — `/api/ecosystem/v1/*`

Plain HTTP on the local listener (53327); outbound peer calls use the TLS
proxy (53328).

| Endpoint | Method | Purpose |
|---|---|---|
| `/info` | GET | Discovery probe — identity, capabilities, FastSwap port |
| `/clipboard/sync` | POST | Receive a clipboard payload (text or base64 media) |
| `/pair/request` | POST | Two-way link handshake (trusts the requester) |
| `/pair/untrust` | POST | Unlink a device |
| `/notification/sync` | POST | Receive a mirrored notification (keyed dedup, icon hash-skip) |
| `/notification/reply` | POST | Forward a reply into the source device's notification |
| `/notification/dismiss` | POST | Mirror a dismissal to linked peers |
| `/notification/action` | POST | Fire an action button of a mirrored notification |
| `/notification/request` | POST | A peer asks this device to re-publish its current notifications |
| `/notification/list` | GET | Current notification snapshot (history) |
| `/call/sync` | POST | Call event (ringing / dialing / talking / ended) from the SIM device |
| `/call/action` | POST | Remote call control (accept / decline / mute / end / dial) |
| `/call/log/sync` | POST | Call-history snapshot push |
| `/call/log/list` | GET | Call-history snapshot |
| `/call/list` | GET | Live call list |
| `/sms/sync` | POST | Receive a mirrored SMS |
| `/sms/send` | POST | Ask the SIM device to transmit an SMS |
| `/sms/list` | GET | SMS snapshot |
| `/contacts/search` | GET | Phonebook search against the phone |
| `/audio/status` | GET | LAN audio session state |
| `/audio/attach` / `/audio/detach` | POST | Open/close the desk-side audio route |
| `/audio/test/tone` | POST | Send a test tone to the handset |

### FastSwap native v1 — `/api/fastswap/v1/*`

| Endpoint | Purpose |
|---|---|
| `GET  /info` | Device identity + download capability probe |
| `POST /register` | Register a transfer peer |
| `POST /prepare-upload` | Session + per-file tokens (receiver approval, 60 s window) |
| `POST /confirm-upload` | Sender waits for the accept/deny decision |
| `POST /upload` | Chunked upload (64 KB chunks, 10 GiB body limit) |

Legacy `/api/localsend/v2/*` paths map to the same handlers for
compatibility with earlier builds and LocalSend-style peers. Received files
are path-sanitized, conflict-renamed (`name (1).ext`), folder structure
preserved, and cancellable per session.

### LAN audio channel

Raw TCP on 53329 — one socket carries the whole duplex call-audio session:

```
initiator → "IGA1" (4B) | u16 BE sample_rate | u16 BE frame_samples | u32 BE 0
listener  → "ACK1"
both      → raw s16le mono frames (48 kHz, 480 samples = 10 ms) until close
```

The phone runs the listener role (Bluetooth-handsfree-style accessory); the
desktop connects. Trust-gated: only linked peers' IPs are accepted.

### Discovery

IPv4 /24 scan across **all usable local interfaces** (private ranges first,
max 4 subnets, own addresses skipped, 192.0.0.0/24 Qualcomm rmnet skipped),
up to 50 concurrent probes against 53327/53328. 30 s heartbeat; untrusted
devices age out after 120 s; trusted devices persist as offline.

---

## Protocol design notes

- **Envelope** — `EcoMessage { version, msg_type, sender_id, sender_name,
  payload, timestamp, signature }` with a typed `MessageType` enum; payloads
  are JSON structs so fields can be added safely.
- **Version compatibility** — protocol v2 fields (rich notifications, media
  clipboard) are all `#[serde(default)]`: old senders omit them and get
  defaults; old receivers ignore unknown fields. The mesh stays mixed-version
  tolerant.
- **Loop-proof clipboard** — SHA-256 content hashing plus two markers
  (`last_content_hash` / `last_applied_hash`); remote-claimed payloads are
  applied at most once, and failed fan-outs are not committed so a retry can
  deliver later. Text goes to `set_text`; base64 media goes through
  `set_media` when the platform supports it.
- **Notification dedup** — stable `notification_key` updates in place;
  per-peer icon cache keyed by MD5 lets peers skip re-sending icon bytes.
- **Identity** — UUID v4 persisted by the host (`pkg/device_id` on desktop,
  DataStore on Android); trust is keyed by that ID, so links survive
  restarts. Self-signed TLS (rcgen) identity lives under `pkg/ecosystem/`
  (`.pem`/`.pub`) and `pkg/certs/` (FastSwap `.der`).
- **Crypto backend** — `rustls`/`tokio-rustls`/`rcgen` are pinned to the
  **ring** provider (no aws-lc-rs), so the Android cross-compile needs no
  CMake/C toolchain beyond the NDK.
- **Runtime** — fully async (tokio); `reqwest` uses `rustls-tls`, never
  native-tls/OpenSSL, and no blocking client — safe to embed in an app.

---

## The UniFFI surface (`src/ffi`)

`EcosystemNode` owns a tokio runtime + `EcoManager` for one app process:

- `EcosystemNode::new(config, clipboard_provider, notification_provider,
  contact_provider, listener)` — applies config, overrides identity, wires
  storage seams (FastSwap certs/downloads), initializes the manager without
  binding ports.
- `start()` — binds eco + FastSwap servers, starts discovery, sync loops,
  and the LAN audio listener. `shutdown()` reverses it.
- Host-injected traits: `ClipboardProvider`, `NotificationProvider`,
  `ContactProvider` (`#[uniffi::export(foreign)]`).
- Pushed events: `NodeEventListener` with `on_device_updated`,
  `on_clipboard_received(_from)`, `on_notification_received` +
  reply/action/dismiss requests, `on_call_received` / `on_call_action`,
  `on_sms_received` / `on_sms_send_requested`, `on_transfer_request` /
  `on_transfer_progress`, `on_audio_frame` / `on_audio_stream_state`.
- Node methods: `pair`, `unpair`, `push_clipboard`,
  `clipboard_delivered`, `push_notification(_rich)`,
  `dismiss_notification`, `push_call`, `push_call_log`, `send_files`,
  `pending_transfers`, `approve_transfer`, `deny_transfer`,
  `get_transfer_progress`, `get_received_files`, `get_devices`,
  `get_notifications`, `get_calls`, `get_sms`, `local_device_id`,
  `trusted_count`.

On Android, `android_logger` + `tracing-log` route core logs to logcat
under the **IGRIS** tag (`adb logcat -s IGRIS`).

---

## Building

### Host (desktop / tests / bindgen library mode)

```bash
cargo build              # debug
cargo build --release    # release
cargo test               # integration tests under tests/
```

`tests/enterprise_api.rs` boots the eco server on a scratch port and
asserts the enterprise route table (info, notification/call/sms lists) and
that unauthenticated call/sms posts are rejected as untrusted rather than
404ing.

### Android cross-compile

```bash
# from this directory, with cargo-ndk + NDK 29 installed
export ANDROID_NDK_HOME=...
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 build --release
```

The Android Gradle build (`igris-andriod`) runs this automatically plus the
bindgen step:

```bash
cargo run --bin uniffi-bindgen -- generate \
  target/aarch64-linux-android/release/libigris_protocol.so \
  -l kotlin -o <android-project>/app/src/main/kotlin --no-format
```

### Crate types

| Type | Consumer |
|---|---|
| `cdylib` | Android `.so` (JNA), any FFI host |
| `staticlib` | iOS (planned) |
| `lib` | Desktop (path dependency), host tests, bindgen library mode |

---

## Storage seams

This crate itself has no hardcoded paths. Hosts inject directories:

- `fastswap::set_fastswap_paths(certs_dir, downloads_dir)` — call before
  the FastSwap server binds. Falls back to `./pkg/certs` / `./downloads`
  when unset (the historical desktop defaults).
- Eco storage is rooted at the `pkg_dir` handed to
  `init_eco_manager_async` (desktop: `./pkg`; Android: app data dir).
- `platform::ecosystem::hooks` exposes the audio/call executor hooks the
  desktop registers at startup (`set_audio_fns`, `set_local_call_action`);
  unregistered hooks behave inertly on hosts without those engines.

---

## Related docs

- Umbrella project README: [`../README.md`](../README.md)
- Desktop app: [`../igris-ecosystem/README.md`](../igris-ecosystem/README.md)
- Android app: [`../igris-andriod/README.md`](../igris-andriod/README.md)
