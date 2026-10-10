# Security Policy

Perpetual runs coding-agent CLIs, Git commands, and a local daemon against the
workspace you select. That is intentional local execution, so only use it with
repositories and agent instructions you trust. Review a workspace's trust
status before starting a session, and do not give an agent access to a
repository that contains credentials or other data it should not read.

## Reporting a vulnerability

Please do not open a public issue for a suspected security vulnerability. Use
GitHub's private vulnerability reporting or a private contact with the
repository owner. Include:

- The affected Perpetual version or commit.
- Your operating system, VS Code version, and workspace type.
- A minimal reproduction and the security impact.
- Relevant logs with tokens, credentials, private source, and personal data
  removed.

Do not include access tokens, API keys, private repositories, or unredacted
agent transcripts in a report. Please allow time for the issue to be
investigated before publishing exploit details.

If you believe a token or credential was exposed, revoke it with the provider
immediately, then report the incident with the smallest useful reproduction.

## Data and credentials

Perpetual keeps its daemon state in VS Code's extension storage. Provider
authentication is handled through the configured agent or VS Code's
authentication facilities; do not paste credentials into prompts, issues, or
logs. The daemon's local transport is authenticated, but it is not a security
boundary against other software already running as the same user.

Multi-account metadata contains labels and routing policy only. Codex account
slots authenticate inside separate `CODEX_HOME` directories and Claude slots
inside separate `CLAUDE_CONFIG_DIR` directories. Claude setup tokens are stored
through the operating system credential vault and are injected into only the
selected Claude child process; token values are redacted from Rust debug output
and never returned in account-status responses. Removing a saved slot deletes
its isolated directory, limit state, and vault entry after confirmation.

## Local data storage and privacy

Perpetual stores session transcripts, task metadata, and other workspace
history **locally** on your machine, in the daemon's data directory under VS
Code's extension storage (or `~/.perpetual` when the daemon is run directly).
This data is written as SQLite records and is **not** encrypted at rest by the
application; do not assume application-level encryption of transcripts,
search indexes, or backups.

The daemon data directory and its discovery endpoint are created with
restrictive OS permissions/ACLs (owner-only on macOS/Linux; a current-user
private ACL on Windows) so that other local users cannot read the bearer token
or transcripts. These protections do **not** cover:

- backups, snapshots, or full-disk access to the data directory, and
- other software already running as your OS user, which is outside the daemon's
  trust boundary.

The localhost loopback authentication on the daemon socket prevents other
machines and other OS users from driving the agent, but it is **not** a
boundary against malicious software already running under the same account.
Review what you run locally, and treat the data directory as sensitive.

## Agent autonomy

`autonomous` permission is an **intentional high-trust option**. When selected,
it may cause the configured provider to run commands without the usual approval
or sandbox prompts. It must never silently become the default, and provider
fallback/switch behavior must not escalate a session to `autonomous` without
your explicit approval. Prefer `workspace_write` (the default) or `read_only`
unless you fully trust the repository and the instructions you are giving the
agent.

Do not paste credentials, API keys, or private source into prompts, issues, or
logs. An agent with authorized workspace access may read sensitive files in
that workspace; connect only repositories you trust it to read.

## Release provenance

The extension may download or bundle a native daemon binary. Presence of the
bundled binary (the `check-daemon` packaging check) verifies that the file is
present, not that it was built from the published source. Until a
release-provenance mechanism (build-from-commit in trusted CI, per-artifact
hashes, and signed attestations) is in place, do not assume binary-to-source
equivalence for bundled daemon artifacts.

## Release audit

Run both `npm audit --omit=dev --audit-level=high` and `npm run audit:rust`
before publishing. The Rust audit script has a narrow, checked exception for
`RUSTSEC-2023-0071`: SQLx's optional MySQL/Postgres drivers remain in
`Cargo.lock`, but this workspace enables SQLite only. The script first verifies
that `rsa` is absent from every enabled target graph and then ignores only that
unresolved advisory. If a future feature enables those drivers, the script
fails and the exception must be removed or replaced with a fixed dependency.
