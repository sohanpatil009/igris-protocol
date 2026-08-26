//! Platform seams for the shared protocol core.
//!
//! Unlike the desktop crate this was forked from, there are **no `#[cfg]`
//! factories** here — the concrete implementations are injected by the host
//! (Android via UniFFI, iOS later, or the desktop shell after the converge
//! step). This module holds only the trait definitions the `eco` core depends on.

pub mod ecosystem;
