//! Host-injected platform hooks.
//!
//! `igris-protocol` stays platform-neutral: capabilities that talk to
//! OS-specific services (the desktop's LAN audio engine, the desktop's own
//! Bluetooth telephony line) are modeled as plain function pointers the host
//! binary registers once at startup. Unregistered hooks degrade gracefully —
//! status hooks read false, action hooks fail with an honest error — so the
//! protocol crate never needs a cfg-gate per OS feature and route handlers
//! stay identical on every build.

use crate::eco::errors::{EcoError, EcoResult};
use std::sync::OnceLock;

// ---- LAN audio engine hooks (desktop) ----

pub type AudioLinkUpFn = fn() -> bool;
pub type AudioAttachFn = fn(std::net::IpAddr) -> Result<(), String>;
pub type AudioDetachFn = fn();
pub type AudioTestToneFn = fn() -> bool;

static AUDIO_LINK_UP: OnceLock<AudioLinkUpFn> = OnceLock::new();
static AUDIO_ATTACH: OnceLock<AudioAttachFn> = OnceLock::new();
static AUDIO_DETACH: OnceLock<AudioDetachFn> = OnceLock::new();
static AUDIO_TEST_TONE: OnceLock<AudioTestToneFn> = OnceLock::new();

/// Install the host audio seam once at node startup (desktop registers its
/// cpal/rodio engine; the phone has no sender and leaves this unset).
pub fn set_audio_fns(link_up: AudioLinkUpFn, attach: AudioAttachFn, detach: AudioDetachFn, test_tone: AudioTestToneFn) {
    let _ = AUDIO_LINK_UP.set(link_up);
    let _ = AUDIO_ATTACH.set(attach);
    let _ = AUDIO_DETACH.set(detach);
    let _ = AUDIO_TEST_TONE.set(test_tone);
}

/// True when a LAN audio link is active. Phones without the engine report
/// `false`, which matches the "no session" REST response.
pub fn audio_link_up() -> bool {
    AUDIO_LINK_UP.get().map(|f| f()).unwrap_or(false)
}

/// Attach the host's audio link to a peer. Unregistered → explicit error.
pub fn audio_attach(ip: std::net::IpAddr) -> Result<(), String> {
    match AUDIO_ATTACH.get() {
        Some(f) => f(ip),
        None => Err("no audio engine on this device".to_string()),
    }
}

/// Detach the host's audio link. No-op when unregistered.
pub fn audio_detach() {
    if let Some(f) = AUDIO_DETACH.get() {
        f();
    }
}

/// Send a test tone over the active link. Unregistered → false.
pub fn audio_test_tone() -> bool {
    AUDIO_TEST_TONE.get().map(|f| f()).unwrap_or(false)
}

// ---- Local call action hook (desktop Bluetooth phone line) ----

pub type LocalCallActionFn = fn(call_id: &str, action: &str, phone_number: &str) -> EcoResult<()>;

static LOCAL_CALL_ACTION: OnceLock<LocalCallActionFn> = OnceLock::new();

/// Install the host's own telephony line once at node startup (desktop).
pub fn set_local_call_action(f: LocalCallActionFn) {
    let _ = LOCAL_CALL_ACTION.set(f);
}

/// Execute a call action on the host's own line. Unregistered → error, so
/// callers fall through to remote (transport) control semantics.
pub fn execute_local_call_action(call_id: &str, action: &str, phone_number: &str) -> EcoResult<()> {
    match LOCAL_CALL_ACTION.get() {
        Some(f) => f(call_id, action, phone_number),
        None => Err(EcoError::Call("no local telephony seam".to_string())),
    }
}
