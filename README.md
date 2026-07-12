# Perpetual for VS Code

Perpetual is an open-source VS Code extension for running and coordinating Claude Code and Codex sessions without leaving your editor. It gives each session a persistent workbench, isolated workspace state, model controls, handoffs, approvals, repository context, and a reviewable change set.

![Perpetual workbench preview](media/PerpetualDemoImage.png)

> Perpetual is under active development. Interfaces and provider behavior may change before the first stable release.

## What it does

- Run Claude Code and Codex sessions side by side from a single workbench.
- Switch agents when a provider reaches a usage limit while retaining task context.
- Choose models, reasoning effort, permission posture, and host or Docker Sandbox execution.
- Connect local repositories or GitHub repositories and review changes in VS Code.
- Keep longer work organized with sessions, work nodes, plans, handoffs, transcripts, and queued follow-ups.
- Use Ollama or LM Studio as local Codex model targets when configured.
- Continue eligible work through provider cloud handoffs and reclaim the result locally.

## Requirements

- VS Code 1.99 or newer.
- Node.js 20 or newer for development and packaging.
- At least one supported provider CLI, installed and authenticated:
  - [Claude Code](https://docs.anthropic.com/en/docs/claude-code/overview)
  - [Codex CLI](https://github.com/openai/codex)
- A trusted VS Code workspace for repository access and local agent execution.

Optional capabilities require their own installation and authentication:

- Docker Sandbox for isolated Codex runs.
- Ollama or LM Studio for local model execution.
- GitHub CLI or a GitHub sign-in for GitHub repository workflows.

## Install

When a release is published, install Perpetual from the VS Code Marketplace or the release `.vsix` file:

```sh
code --install-extension ./perpetual-vscode-<target>-<version>.vsix
```

After installation, open the Perpetual icon in the Activity Bar. Select an agent, choose the permission and execution settings, connect a repository if needed, and start a session.

## Permissions and safety

Perpetual keeps the daemon local to the extension host. The bundled `am-daemon` process owns the SQLite database, agent subprocesses, worktrees, and local authenticated JSON-RPC socket used by the extension. It does not expose a public network service.

Permission choices are explicit:

- `Read only` asks the provider to plan or inspect without writing.
- `Workspace write` allows normal workspace edits while retaining provider safeguards.
- `Ask` enables live approval for Codex app-server actions.
- `Autonomous` opts into the provider's full-automation mode.

Claude Code's headless CLI does not expose the same interactive approval
channel, so its normal and ask-style runs use the provider's non-interactive
permission mode; Codex is the adapter with live in-app approval cards.

Treat `Autonomous` and cloud handoff settings as high-trust options. Review the provider's own authentication, billing, and permission documentation before enabling them.

## Development

Clone the repository and install the locked dependency set:

```sh
git clone https://github.com/SakethSripada/AgentManagerVSCodeExtension.git
cd AgentManagerVSCodeExtension
npm ci
```

Run the checks used during development:

```sh
npm test
cargo test --workspace
npm run build
```

Build and install a local extension for the current platform:

```sh
npm run build:daemon -- --target=darwin-arm64  # choose your target
npm run copy-daemon -- --target=darwin-arm64
npm run package:darwin-arm64
```

Or use the development helper, which builds, packages, installs, and reopens the current workspace:

```sh
npm run install:local
```

The supported daemon targets are `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`, and `win32-arm64`. Cross-platform builds need the Rust target, a compatible linker, and the native C toolchain required by dependencies such as `ring`.

The ignored live-agent test uses the real Codex CLI and may consume provider quota:

```sh
cargo test -p am-daemon --test live_approval -- --ignored --nocapture --test-threads=1
```

## Architecture

```text
VS Code extension
        │ authenticated localhost JSON-RPC
        ▼
am-daemon ── am-core ── am-agents ── Claude Code / Codex
    │          │
    │          ├── SQLite state and migrations
    │          ├── worktrees and repository operations
    │          └── local/cloud/sandbox orchestration
    ▼
power and process lifecycle management
```

The repository is intentionally split into small Rust crates:

| Crate | Responsibility |
| --- | --- |
| `am-proto` | Shared wire and domain types |
| `am-db` | SQLite connection, migrations, and repositories |
| `am-vcs` | Git worktrees, diffs, commits, and repository helpers |
| `am-agents` | Claude Code and Codex adapters plus event normalization |
| `am-core` | Orchestration, scheduling, policy, approvals, and handoffs |
| `am-daemon` | Headless process and authenticated local transport |

## Repository layout

```text
src/                 Extension host and daemon client
webview/              React workbench UI
landing/              Standalone Perpetual marketing site
crates/               Rust daemon workspace
media/                Extension icons and product preview
scripts/              Build, packaging, and daemon lifecycle helpers
```

## Troubleshooting

- If the extension cannot find a daemon, run the target-specific build and copy commands above, or set `perpetual.daemonPath`.
- If an agent is unavailable, install its CLI and authenticate it in the same environment used by VS Code.
- Repository and local CLI features require a trusted workspace; Restricted Mode intentionally limits them.
- Use the `Perpetual` output channel for daemon startup, authentication, and subprocess diagnostics.
- For support questions, see [SUPPORT.md](SUPPORT.md). For security reports, see [SECURITY.md](SECURITY.md).

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Please include a focused description, tests for behavior changes, and any platform-specific packaging notes.

## License

Perpetual is released under the [MIT License](LICENSE). See [NOTICE.md](NOTICE.md) for third-party notices and [PUBLISHING.md](PUBLISHING.md) for release and Marketplace packaging notes.
