# Security Policy

AgentManager executes local coding-agent CLIs and Git commands in managed
workspaces. Only run agent sessions in repositories you trust.

To report a vulnerability, open a private security advisory or email the
maintainers listed by the AgentManager project. Do not publish exploit details
until a fix is available.

GitHub tokens are requested through VS Code's built-in authentication provider
and are passed to the daemon only for the active operation. They are not stored
in AgentManager's SQLite database.
