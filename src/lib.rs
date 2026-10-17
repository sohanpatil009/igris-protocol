//! igris-protocol — shared LAN device-ecosystem protocol core.
//!
//! Seeded by forking the `eco/` + `fastswap/` modules out of `igris-ecosystem`
//! (the Rust desktop app) into a standalone, cross-compilable library so the
//! Android (and later iOS) companions drive the *same* wire protocol as the
//! desktop instead of re-implementing it. The host injects platform behavior
//! (clipboard, notifications) through the traits in [`platform`]; the UniFFI
//! surface in `ffi` (added in Phase 1) wraps this for Kotlin.
//!
//! Wire protocols (Igris native, enterprise):
//! - Eco REST: `/api/ecosystem/v1/*` (clipboard + notification sync, pairing, calls, SMS)
//! - FastSwap: `/api/fastswap/v1/*` (native file transfer)

pub mod eco;
pub mod fastswap;
pub mod ffi;
pub mod platform;

// Phase 1: UniFFI scaffolding — generates the C ABI entry points the
// Kotlin side calls through (via JNA on Android). Requires the crate name
// to match the bindgen namespace (igris_protocol).
uniffi::setup_scaffolding!();
