//! LAN audio channel (Igris native).
//!
//! One TCP connection carries the whole duplex audio session (both mics
//! and both speakers). After the handshake every FRAME_BYTES-sized chunk
//! is one frame — fixed size means no per-frame length prefix.
//!
//! Handshake (initiator → listener):
//! ```text
//! "IGA1" (4B) | u16 BE sample_rate | u16 BE frame_samples | u32 BE reserved(0)
//! ```
//! Listener answers `b"ACK1"`, then both sides push raw s16le mono frames
//! until either side closes.
//!
//! Identical in both crates: the phone runs the listener role, the
//! desktop the initiator role, but both roles exist here so any device
//! can initiate.

#![allow(unused)]

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

pub const AUDIO_PORT: u16 = 53329;
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 480; // 10 ms mono s16
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;

const HELLO_MAGIC: &[u8; 4] = b"IGA1";
const ACK: &[u8; 4] = b"ACK1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioParams {
    pub sample_rate: u16,
    pub frame_samples: u16,
}

impl AudioParams {
    pub const fn default_params() -> Self {
        Self {
            sample_rate: SAMPLE_RATE as u16,
            frame_samples: FRAME_SAMPLES as u16,
        }
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_samples as usize * 2
    }

    fn to_hello(&self) -> [u8; 12] {
        let mut h = [0u8; 12];
        h[0..4].copy_from_slice(HELLO_MAGIC);
        h[4..6].copy_from_slice(&self.sample_rate.to_be_bytes());
        h[6..8].copy_from_slice(&self.frame_samples.to_be_bytes());
        h // reserved u32 stays zero
    }

    fn parse(buf: &[u8; 12]) -> Option<Self> {
        if &buf[0..4] != HELLO_MAGIC {
            return None;
        }
        let rate = u16::from_be_bytes(buf[4..6].try_into().ok()?);
        let samples = u16::from_be_bytes(buf[6..8].try_into().ok()?);
        if rate == 0 || samples == 0 {
            return None;
        }
        Some(Self {
            sample_rate: rate,
            frame_samples: samples,
        })
    }
}

type FrameTx = tokio::sync::mpsc::UnboundedSender<Vec<u8>>;

/// Storage for the outbound write half of the ACTIVE link. A new link
/// replaces the previous one (single-caller session semantics; last link
/// wins, and teardown clears).
pub struct ActiveLink {
    tx: Mutex<Option<FrameTx>>,
    params: Mutex<Option<AudioParams>>,
}

impl ActiveLink {
    pub fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            params: Mutex::new(None),
        }
    }

    fn set(&self, tx: FrameTx, params: AudioParams) {
        if let Ok(mut g) = self.tx.lock() {
            *g = Some(tx);
        }
        if let Ok(mut g) = self.params.lock() {
            *g = Some(params);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.tx.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.params.lock() {
            *g = None;
        }
    }

    pub fn params(&self) -> Option<AudioParams> {
        self.params.lock().ok().and_then(|g| *g)
    }

    /// Queue frames for the peer. Quiet drop when no link is up —
    /// realtime audio never blocks by design.
    pub fn send(&self, frames: Vec<u8>) -> bool {
        let (tx, fb) = match (self.tx.lock(), self.params.lock()) {
            (Ok(g), Ok(p)) => match (&*g, &*p) {
                (Some(tx), Some(p)) => (tx.clone(), p.frame_bytes()),
                _ => return false,
            },
            _ => return false,
        };
        if fb == 0 || frames.len() % fb != 0 {
            return false;
        }
        tx.send(frames).is_ok()
    }
}

pub type FrameSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
pub type StateSink = Arc<dyn Fn(bool) + Send + Sync>;
pub type TrustedIp = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

/// Serve inbound audio links (listener role). `on_frame` carries peer
/// frames, `on_state` fires true on open / false on close. A later accept
/// replaces the active outbound target (last link wins).
pub async fn listen(
    audio: Arc<ActiveLink>,
    is_trusted: TrustedIp,
    on_frame: FrameSink,
    on_state: StateSink,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", AUDIO_PORT))
        .await
        .map_err(|e| format!("audio bind {AUDIO_PORT} failed: {e}"))?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        if !is_trusted(peer.ip()) {
            tracing::info!("[ECO] audio: rejected untrusted peer {peer}");
            continue;
        }
        let mut socket = socket;
        let mut hello = [0u8; 12];
        if socket.read_exact(&mut hello).await.is_err() {
            tracing::info!("[ECO] audio: handshake read failed from {peer}");
            continue;
        }
        let params = match AudioParams::parse(&hello) {
            Some(p) => p,
            None => {
                tracing::info!("[ECO] audio: bad hello magic from {peer}");
                continue;
            }
        };
        if socket.write_all(ACK).await.is_err() {
            tracing::info!("[ECO] audio: ack write failed for {peer}");
            continue;
        }
        // Replace any previous link, then run the reader inline; the
        // writer pumps the outbound queue on its own task.
        audio.clear();
        let (rd, wr) = socket.into_split();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        audio.set(tx.clone(), params);
        let open_state = on_state.clone();
        open_state(true);
        let writer = tokio::spawn(async move {
            let mut wr = wr;
            while let Some(frame) = rx.recv().await {
                if wr.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });
        let mut rd = rd;
        let mut buf = vec![0u8; params.frame_bytes()];
        loop {
            match rd.read_exact(&mut buf).await {
                Ok(_) => on_frame(buf.clone()),
                Err(_) => break,
            }
        }
        // Close teardown: dropping the sender half ends the writer task.
        drop(open_state);
        audio.clear();
        drop(tx);
        let _ = writer.await;
        on_state(false);
    }
}

/// Connect an outbound audio link (initiator role). Same duplex shape as
/// the listener side after the handshake. Returns when the link dies.
pub async fn connect(
    audio: Arc<ActiveLink>,
    addr: std::net::SocketAddr,
    params: AudioParams,
    on_frame: FrameSink,
    on_state: StateSink,
) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| format!("audio connect {addr}: {e}"))?;
    sock.write_all(&params.to_hello())
        .await
        .map_err(|e| format!("audio hello: {e}"))?;
    let mut acked = [0u8; 4];
    sock.read_exact(&mut acked)
        .await
        .map_err(|e| format!("audio ack: {e}"))?;
    if &acked != ACK {
        return Err("audio handshake refused".to_string());
    }
    let (rd, mut wr) = sock.into_split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    audio.set(tx, params);
    on_state(true);
    let audio_link = audio.clone();
    let mut close_state = on_state.clone();
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
        audio_link.clear();
        close_state(false);
    });
    let mut rd = rd;
    let mut buf = vec![0u8; params.frame_bytes()];
    loop {
        match rd.read_exact(&mut buf).await {
            Ok(_) => on_frame(buf.clone()),
            Err(_) => break,
        }
    }
    let _ = writer.await;
    Ok(())
}
