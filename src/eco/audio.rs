//! Call audio routing decisions (Igris native).
//!
//! A desktop-initiated answer on a linked handset can also carry the wish
//! that the call's audio be carried by the desk machine's microphone and
//! speakers (the handset pairs the desk machine as its Bluetooth handsfree
//! accessory). Android exposes that switch in two OS-level bits
//! ("apply routing now" / "clear routing now"), which only make sense once
//! the line is actually offhook — but an answer may beat the telephony
//! transition. So the request parks here and both decisions fire on the
//! next local call event pushed up from the handset sensor layer.
//!
//! The module is shared by both crates: the handset drives it from the
//! call-action receiver and the local call-event stream; the desk side
//! never enters a route (it only relays the request).

#![allow(unused)]

use std::sync::atomic::{AtomicU8, Ordering};

/// Desired routing action for the latest pushed local call event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioRouteDecision {
    /// Line went live: apply the pending route now.
    Apply,
    /// Line is over: drop any route now.
    Clear,
    /// Nothing to do.
    None,
}

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const APPLIED: u8 = 2;

static AUDIO_STATE: AtomicU8 = AtomicU8::new(IDLE);

/// A peer asked this handset to carry its call audio. Accepts `audio`
/// (route when the line goes live) and `audio_off` (drop any pending
/// request). Returns `true` when the action was consumed here and must
/// not be forwarded to the platform action executor.
pub fn peer_audio_request(action: &str) -> bool {
    match action {
        "audio" => {
            AUDIO_STATE.store(PENDING, Ordering::SeqCst);
            true
        }
        "audio_off" => {
            AUDIO_STATE.store(IDLE, Ordering::SeqCst);
            true
        }
        _ => false,
    }
}

/// Pure decision on the local call-event stream (`ringing | dialing |
/// talking | ended | ...` pushed up by the handset sensor bridge).
pub fn on_call_event(event: &str) -> AudioRouteDecision {
    match event {
        "talking" => {
            let prev = AUDIO_STATE.swap(APPLIED, Ordering::SeqCst);
            if prev == PENDING {
                AudioRouteDecision::Apply
            } else {
                if prev != APPLIED {
                    AUDIO_STATE.store(prev, Ordering::SeqCst);
                }
                AudioRouteDecision::None
            }
        }
        "ended" | "rejected" => {
            let prev = AUDIO_STATE.swap(IDLE, Ordering::SeqCst);
            if prev == APPLIED {
                AudioRouteDecision::Clear
            } else {
                if prev != IDLE {
                    AUDIO_STATE.store(prev, Ordering::SeqCst);
                }
                AudioRouteDecision::None
            }
        }
        _ => AudioRouteDecision::None,
    }
}
