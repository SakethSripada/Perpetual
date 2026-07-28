# Multi-device collaboration

Perpetual can coordinate Claude Code and Codex installations across multiple
computers on the same local network. Each computer uses its own locally
installed CLI and authenticated provider account. The host keeps the shared
task database, transcript, repository identities, and review queue.

## Connect devices

1. Open the **Devices** button in Perpetual on the computer that should host
   the shared workspace.
2. Choose **Share from this device**. Perpetual starts an encrypted LAN listener
   and copies a single-use invite that expires after 15 minutes.
3. Open **Devices** on another computer, paste the invite, and choose **Join**.
4. Repeat the join step for additional computers. There is no two-device limit;
   the host accepts up to 32 simultaneous encrypted connections.

The host must remain open and reachable while members work. Reconnected devices
use a device-specific credential in VS Code Secret Storage, so the invite is
needed only for the first connection or after revocation. Set a friendly name
with `perpetual.collaboration.deviceName`; otherwise Perpetual uses the hostname.

## Run work on another computer

The composer gains a device picker after the workspace is shared. Select a
ready device and agent, then send the prompt normally. A member computer treats
**This device** as its own local Claude/Codex installation; the host keeps a
separate direct **This device** route.

Perpetual then:

- atomically assigns and leases the turn to the selected installation;
- creates a hidden managed worktree there instead of editing its visible clone;
- builds a compact handoff from task state and a small recent-activity window;
- streams normalized progress into the shared transcript with the device name;
- queues follow-up prompts into the same remote run;
- relays Codex approval cards and returns decisions to the leased worker; and
- returns a bounded binary Git patch for host review.

Provider-native session IDs, login state, API credentials, and account data are
not copied to the coordinator. Each invocation consumes usage only on the device
and account that actually runs it.

## Repository matching and collision safety

Every worker needs a local clone corresponding to each repository attached to
the shared session. Keep those clones open as VS Code workspace folders on the
worker. Perpetual matches clones by normalized Git remote URL, then by repository
name, with a single-clone fallback when the mapping is unambiguous.

Write assignments acquire a coordinator-side repository lease. Shared agents
can work concurrently on different repositories, but only one owns a given
repository until its returned changes are resolved. Read-only assignments do
not take a writer lease.

Returned work is never silently copied over the host checkout:

- **Apply** checks affected paths for uncommitted host edits and verifies the
  patch.
- **Conflict** lists overlapping files and keeps the peer patch pending.
- **Reject** discards that returned change set.
- **Overwrite Local Files** is an explicit host-only decision. Perpetual
  materializes the patch at its recorded base, replaces only declared files,
  and retains their previous contents under
  `collaboration-backups/<change-set-id>` in the daemon data directory.

Workers renew a short fencing lease while preparing and running. Two failed
renewals stop the local agent, avoiding provider usage on work the coordinator
can no longer accept.

## Security model

- LAN traffic uses AES-256-GCM with per-session keys derived by HKDF-SHA256 and
  a mutually authenticated HMAC handshake.
- 256-bit invite secrets expire after 15 minutes and become device-specific
  reconnect credentials after pairing.
- Credentials live in VS Code Secret Storage, never settings or shared SQLite.
- Frames have monotonic counters and strict size/concurrency limits.
- The proxy exposes an allowlist of shared-workspace RPCs rather than the full
  daemon surface, and binds registration, heartbeat, and claims to the
  authenticated device ID.
- Coordinator lease tokens are stored only as SHA-256 hashes.
- Credential-shaped fields in relayed approvals are redacted.
- Revocation removes the reconnect credential, disconnects sessions, cancels
  active assignments, and fences late output.

Pair only computers you control. Paired devices can see shared prompts,
transcripts, repository metadata, approvals, and returned source patches, and
can create or update shared sessions. The daemon remains loopback-only; only
the explicitly enabled collaboration proxy listens on the LAN.

## Usage efficiency

Coordination makes no extra model calls. Handoffs are capped at 24 KiB and use
structured task state plus at most eight recent events instead of full provider
history. Streaming text is coalesced to 250 ms, normal UI snapshots omit patch
bodies, and approved patches synchronize state without entering the next model
prompt. Local polling (2.5 seconds) and heartbeats (15 seconds) consume no model
tokens.

## Troubleshooting

- Both computers must be on the same reachable IPv4 LAN. Guest Wi-Fi and VPN
  client isolation can block peers.
- Allow VS Code/Perpetual through the host firewall when prompted.
- Keep the host workspace and member VS Code window open for reconnect.
- For a missing-repository error, open a matching clone as a workspace folder
  on that worker. The **Needs attention** card can add the clone and retry the
  assignment without duplicating its prompt.
- If more than one open clone has the same remote or name, keep only the clone
  intended for the run in that VS Code workspace. Perpetual stops instead of
  choosing one arbitrarily.
- Authenticate the selected CLI on the selected device; accounts need not
  match the host account.
- Use the **Perpetual** output channel for connection, mapping, lease, and patch
  diagnostics.
