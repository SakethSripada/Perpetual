# Perpetual for VS Code

Perpetual is a local VS Code workbench for running Claude Code and Codex on
the same task. It keeps session context, repository attachments, queued turns,
approvals, and usage-limit handoffs in one place so you can move between agents
without rebuilding state.

![Perpetual workbench](media/screenshot-workbench.png)

## Features

- Chat-first Perpetual workbench in the Activity Bar, with the same view
  available as a wide VS Code panel.
- Per-session agent routing for Claude Code and Codex, including permission,
  model, reasoning, and runtime controls.
- Automatic handoff when an agent hits a usage limit, with configurable fallback
  order, recovery behavior, and retry timing.
- Codex can be pointed at local Ollama or LM Studio models for local-model
  fallback/continuation when the bundled daemon supports it.
- Local repository attachment plus GitHub repository connection through VS Code's
  built-in GitHub authentication.
- Managed workspaces for agent edits, with inline changed-file summaries and
  quick access to generated worktrees.
- Queued follow-up turns while an agent is running, including queue editing and
  reordering.
- Approval prompts for commands, tools, and file changes that need user consent.
- Codex runs on the host or inside Docker Sandbox through Docker's `sbx` CLI.

## Requirements

- VS Code 1.99 or newer.
- Claude Code and/or Codex CLI installed and authenticated on the machine where
  the workspace extension runs.
- Optional Docker Sandbox support requires Docker plus the `sbx` CLI.

## Getting Started

1. Open the Perpetual icon in the Activity Bar.
2. Attach the current workspace repository, add a local folder, or connect a
   GitHub repository.
3. Pick Claude or Codex, then choose the permission mode and optional run
   settings.
4. Send a message. If a run is already active, the message is queued as the next
   turn.

Perpetual creates managed workspaces for agent edits so your original working
tree stays reviewable.

Use **Perpetual: Open Perpetual Panel** from the Command Palette when you
want the workbench in VS Code's bottom panel instead of the side bar.

## Configuration

The extension contributes `agentmanager.*` settings for defaults and runtime
policy:

- `defaultAgent`, `defaultPermission`, `defaultExecutionBackend`,
  `defaultModel`, `defaultReasoning`, `defaultLocalProvider`, and
  `defaultLocalBaseUrl` control new sessions.
- `autoSwitchOnLimit`, `switchBackOnRecovery`, `fallbackPriority`,
  `resumeWithEarliestAgent`, and `unknownLimitRetrySeconds` control handoff
  behavior.
- `sandbox.maxConcurrent`, `sandbox.cpus`, `sandbox.memory`, and
  `sandbox.networkPreset` control Docker Sandbox runs.
- `daemonPath` can point at a custom `am-daemon` binary. Empty uses the binary
  bundled with the extension.

## Privacy and Security

Perpetual runs locally. Session data, transcripts, and managed workspaces are
stored under the extension's VS Code global storage directory. GitHub access uses
VS Code's built-in GitHub authentication; Perpetual does not store GitHub
OAuth tokens in its database.

The extension executes local CLIs and repository operations, so Workspace Trust
is required before running agents or attaching repositories.

## Repository layout

This is a standalone repository for the VS Code extension. Its TypeScript,
webview code, and vendored daemon Rust workspace live here and are edited here
directly.

The daemon workspace includes `am-daemon`, `am-core`, `am-agents`, `am-mcp`,
`am-db`, `am-proto`, `am-compute`, `am-policy`, and `am-vcs`. The bundled
`am-daemon` binaries under `bin/<target>/` are build artifacts from that local
workspace. They are committed so the published extension is self-contained and
installs without a Rust toolchain.

## Development

```bash
npm install
npm run build
npm test
```

For iterative work, run `npm run watch:extension` and `npm run watch:webview` in
separate terminals.

## Daemon workflow

When daemon-side behavior changes, build and copy the daemon from this extension
repo:

```bash
npm run build:daemon -- --target=win32-x64
npm run copy-daemon -- --target=win32-x64
```

Replace `win32-x64` with `darwin-arm64`, `darwin-x64`, `linux-x64`,
`linux-arm64`, or `win32-arm64` as needed. The scripts accept both
`--target=win32-x64` and `--target win32-x64`.

Commit the updated Rust source, `Cargo.lock`, and `bin/<target>/` binary before
packaging. The AgentManager app repo keeps its own copy of the crates; changes
do not flow between repos automatically.

## Packaging

`vsce package` builds the extension first (via `vscode:prepublish`) and bundles
the daemon for the chosen target. The `check-daemon` step fails early with the
local build command if a target's binary is missing or if it does not include
local model fallback and diff support.

```bash
npm install
npm run build:daemon -- --target=win32-x64
npm run copy-daemon -- --target=win32-x64
npm run package:win32-x64
```

See [PUBLISHING.md](PUBLISHING.md) for the full Marketplace checklist.
