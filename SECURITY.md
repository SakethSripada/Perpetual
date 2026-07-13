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

## Release audit

Run both `npm audit --omit=dev --audit-level=high` and `npm run audit:rust`
before publishing. The Rust audit script has a narrow, checked exception for
`RUSTSEC-2023-0071`: SQLx's optional MySQL/Postgres drivers remain in
`Cargo.lock`, but this workspace enables SQLite only. The script first verifies
that `rsa` is absent from every enabled target graph and then ignores only that
unresolved advisory. If a future feature enables those drivers, the script
fails and the exception must be removed or replaced with a fixed dependency.
