# Support

Use the [Perpetual issue tracker](https://github.com/SakethSripada/Perpetual/issues)
for reproducible bugs and feature requests. Choose the matching issue template
when one is available.

## Before opening an issue

1. Update Perpetual and reload VS Code with **Developer: Reload Window**.
2. Confirm the workspace is trusted and that the required agent CLI is
   installed and available on `PATH`.
3. Open **View → Output**, select **Perpetual**, and copy the relevant error
   and daemon startup lines.
4. Retry once and note whether the problem affects a new session, an existing
   session, or only a particular workspace.

If the daemon cannot be found during development, build and copy the matching
target binary or set `perpetual.daemonPath`; released extensions normally use
the bundled daemon.

## Include in a report

- Perpetual version and installation source.
- VS Code version, operating system, CPU architecture, and extension host
  type.
- Workspace type: local, SSH, WSL, Dev Container, or Codespaces.
- Agent CLI names and versions, plus the selected model if relevant.
- Exact steps to reproduce, expected behavior, and actual behavior.
- Relevant Perpetual output with secrets and private code removed.

Do not attach API keys, access tokens, private repository contents, full
environment dumps, or unredacted agent transcripts. For a suspected security
issue, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.
