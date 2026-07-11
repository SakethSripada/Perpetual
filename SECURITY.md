# Security Policy

AgentManager executes local coding-agent CLIs and Git commands in managed
workspaces. Only run agent sessions in repositories you trust.

To report a vulnerability, open a private security advisory or email the
maintainers listed by the AgentManager project. Do not publish exploit details
until a fix is available.

GitHub tokens are requested through VS Code's built-in authentication provider
and are passed to the daemon only for the active operation. They are not stored
in AgentManager's SQLite database.

## Release audit

Run both `npm audit --omit=dev --audit-level=high` and `npm run audit:rust`
before publishing. The Rust audit script has a narrow, checked exception for
`RUSTSEC-2023-0071`: SQLx's optional MySQL/Postgres drivers remain in
`Cargo.lock`, but this workspace enables SQLite only. The script first verifies
that `rsa` is absent from every enabled target graph and then ignores only that
unresolved advisory. If a future feature enables those drivers, the script
fails and the exception must be removed or replaced with a fixed dependency.
