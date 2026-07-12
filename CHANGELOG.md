# Changelog

## Unreleased

- Remove the unused embedded MCP server and bridge from the daemon workspace.
- Refresh the open-source documentation, contributor guidance, and workbench preview.

## 0.1.0

- Initial production release.
- Adds the Perpetual Workbench sidebar and editor panel.
- Supports Claude Code and Codex session routing through the bundled daemon.
- Supports VS Code GitHub OAuth, local repository attachment, queued turns, and
  Codex Docker Sandbox readiness/sign-in flows.
- Adds bounded interruption recovery, rate-limit switching/switchback, and
  cloud continuity across configured sleep/shutdown lifecycle events.
