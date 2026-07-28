# Changelog

## Unreleased

- Add encrypted LAN collaboration across multiple Perpetual installations and
  independent Claude Code or Codex accounts, with simple invite pairing,
  persistent device credentials, revocation, presence, and device-aware agent
  selection.
- Share bounded handoff prompts, live progress, follow-up turns, and Codex
  approvals without additional model calls.
- Run remote work in isolated managed worktrees with fenced leases,
  coordinator-side repository writer locks, host review, conflict detection,
  and recoverable explicit overwrite.

## 0.2.2

- Document token and weekly-percentage task budgets in the README and
  Marketplace listing, and add the session budget screenshot.

## 0.2.1

- Start new-thread runs immediately instead of waiting for a full refresh.
- Remember the last agent, model, and reasoning selection for new sessions.
- Keep the Working indicator visually consistent from its first frame.
- Keep provider task budgets aligned with reliable telemetry: Claude token-only
  budgets and Codex weekly-usage budgets.

## 0.2.0

- Add graceful per-session token targets and Codex weekly-usage percentage
  targets with private usage reconciliation, closeout guidance, and pause-safe
  handoffs.
- Add static task-budget controls to the composer and document provider
  limitations and response-boundary behavior.

## 0.1.5

- Resolve the agent CLI by newest installed version instead of first directory
  hit: machines with a stale npm-global Codex next to the auto-updating Codex
  desktop-app/IDE-extension install were pinned to the old CLI, hiding new
  models (e.g. GPT-5.5 and the GPT-5.6 Sol/Terra/Luna family) from the picker.
  The Codex app's managed install directory now participates in discovery.

_0.1.4 was tagged but never published; its changes ship in 0.1.5._

- Detect the Codex model catalog live from the installed CLI over the
  app-server `model/list` RPC (the removed `codex debug models` subcommand is
  kept only as a fallback for older CLIs), so new models and their per-model
  reasoning efforts appear without an extension update.
- Show the Claude lineup with proper versioned names (Claude Fable 5, Claude
  Opus 4.8/4.7/4.6, Claude Sonnet 5, Claude Sonnet 4.6, Claude Haiku 4.5) and
  fold the CLI's `fable`/`opus`/`sonnet`/`haiku` aliases into those entries.
- Offer only the reasoning efforts each model actually supports (e.g. no
  `xhigh` on the 4.6 generation, Default-only for Haiku 4.5) and read the
  supported effort levels from `claude --help` so future levels are picked up
  automatically.
- Refresh the built-in fallback model lists shown when no CLI is installed.
- Keep custom model ids typed into the picker flowing through to both CLIs
  unchanged.
- Anchor popover menus (including Run options) to the trigger's fixed edge so
  they no longer open slightly offset to the left.

## 0.1.3

- Add a focused Marketplace README highlighting agent switching, automatic
  limit-reset resume, and cloud continuity.
- Use the Marketplace README when packaging every platform-specific VSIX.
- Satisfy current stable Clippy lints in the agent detection and process helpers.

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
