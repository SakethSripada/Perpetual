# AgentManager for VS Code

AgentManager runs Claude Code and Codex sessions from inside VS Code, preserving
context across agent switches, usage limits, and runtime changes.

## Features

- Chat-first Workbench in the Activity Bar, with an optional wide editor panel.
- Pick Claude Code or Codex per session, including model and reasoning controls.
- Automatic handoff when an agent hits a usage limit, using AgentManager context
  files so the next agent can continue without starting cold.
- Connect the current workspace repository or clone GitHub repositories through
  VS Code's built-in GitHub authentication.
- Queue follow-up turns while an agent is still running.
- Run Codex on Host or in Docker Sandbox through Docker's `sbx` CLI.
- Inspect session status, attached repositories, queued turns, and diffs.

## Requirements

- VS Code 1.99 or newer.
- Claude Code and/or Codex CLI installed and authenticated on the machine where
  the workspace extension runs.
- Optional Docker Sandbox support requires Docker plus the `sbx` CLI.

## Getting Started

1. Open the AgentManager icon in the Activity Bar.
2. Attach the current workspace repository, or connect a GitHub repository.
3. Pick an agent and permission mode.
4. Send a message.

AgentManager creates managed workspaces for agent edits so your original working
tree stays reviewable.

## Privacy and Security

AgentManager runs locally. Session data, transcripts, and managed workspaces are
stored under the extension's VS Code global storage directory. GitHub access uses
VS Code's built-in GitHub authentication; AgentManager does not store GitHub
OAuth tokens in its database.

The extension executes local CLIs and repository operations, so Workspace Trust
is required before running agents or attaching repositories.

## Packaging

This extension bundles an `am-daemon` binary per platform-specific VSIX. To build
the current platform package from the repository root:

```bash
cargo build -p am-daemon --release
cd vscode-extension
npm install
npm run copy-daemon
npm run package
```

Use `npm run package:darwin-arm64`, `npm run package:linux-x64`, or
`npm run package:win32-x64` when publishing platform-specific packages.
