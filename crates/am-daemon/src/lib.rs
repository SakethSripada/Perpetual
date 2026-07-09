//! Headless daemon for AgentManager.
//!
//! Hosts the UI-agnostic [`am_core::AppCore`] in its own process and exposes it
//! over a localhost TCP socket so a desktop UI — or any client — can drive the
//! orchestrator without embedding it. This is the M7 "extract `am-core` into a
//! headless daemon; UI as client" foundation: an always-on background process
//! that owns agent sessions independently of any window.
//!
//! Transport: newline-delimited JSON frames (see [`protocol`]). Connections are
//! authenticated with a shared token and bound to `127.0.0.1` only.

mod client;
pub mod protocol;
mod server;

pub use client::{ClientError, DaemonClient};
pub use server::Server;

use tokio::io::AsyncWriteExt;

/// Write one newline-delimited JSON frame. `serde_json` never emits interior
/// newlines, so a single line is a complete frame.
pub(crate) async fn write_line<W>(
    writer: &mut W,
    value: &impl serde::Serialize,
) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut buf = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    buf.push(b'\n');
    writer.write_all(&buf).await
}

/// Generate a random 32-hex-character session token without pulling in a crypto
/// dependency: seed from the system clock and thread id and expand with a
/// SplitMix64 PRNG. Sufficient to gate a localhost-only socket.
pub fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(17);
    let mut out = String::with_capacity(32);
    for _ in 0..4 {
        // SplitMix64 step.
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out
}
