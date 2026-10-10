//! Headless daemon for Perpetual.
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
pub mod endpoint;
pub mod power;
pub mod protocol;
mod server;

pub use client::{ClientError, DaemonClient};
pub use server::dispatch;
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

/// Generate a fresh 256-bit bearer token as 64 lowercase hex characters.
///
/// The token comes exclusively from the operating system's CSPRNG. If the OS
/// randomness source is unavailable we fail hard rather than falling back to a
/// predictable seed (wall-clock time, PID, or a non-cryptographic PRNG), since
/// the token gates every authenticated RPC on the daemon socket.
pub fn generate_token() -> String {
    use std::fmt::Write as _;

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .expect("secure OS randomness is required for daemon authentication");

    let mut token = String::with_capacity(64);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("formatting into a String cannot fail");
    }
    token
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_tokens_have_expected_format_and_no_duplicates() {
        let tokens: HashSet<_> = (0..1_000).map(|_| generate_token()).collect();
        assert_eq!(tokens.len(), 1_000);
        assert!(tokens.iter().all(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }));
    }
}
