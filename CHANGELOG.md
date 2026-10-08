# Changelog

## Unreleased

## 0.7.12

- Simplify the model and reasoning selector with neutral colours, a smaller panel,
  centred effort/model typography, and a quieter composer pill.
- Move provider switching into a compact menu and use plain model rows with one
  checkmark. Remove repeated reasoning labels and oversized provider tabs.
- Preserve keyboard controls, actual model defaults, provider preferences and
  failed-save recovery; keep model names readable in narrow sidebars.

## 0.7.11

- Redesign model selection as a compact pill and focused popover, with a separate
  model list and a stepped reasoning slider for both Codex and Claude Code.
- Show each model's actual reasoning levels, reset to its reported default, and
  preserve saved choices across providers. Commit slider changes on release.
- Support keyboard navigation, search for long lists, nested Escape navigation,
  missing defaults, single-level models, and models without adjustable reasoning.
- Keep model names readable in the smallest sidebars and respect light/dark themes
  and reduced motion.

## 0.7.10

- Remove GitHub browsing, sign-in and cloning from the extension, including
  the command, settings entry and unused host requests. Select local projects
  through the folder picker; existing local clones remain available.

## 0.7.9

- Combine provider, actual model and supported reasoning choices in a searchable
  composer menu. Remove the Default model option; remember choices per provider,
  restore them across panels, and recover cleanly from failed saves.
- Remove the redundant session status/account subtitle from the chat header.
- Review visible repositories against current HEAD so committed edits clear.
  Include staged, unstaged and untracked edits without modifying the Git index;
  retain committed task changes in isolated worktrees until applied.
- Refresh open repository reviews on Git changes with debounced, coalesced reads.
  Label review scopes explicitly and retain readable narrow repository controls.

## 0.7.8

- Replace the ambiguous repository button with a labeled, searchable picker.
  Separate chat selection from connection actions and confirm disconnections inline.
- Share structured usage allowances and reset times across Accounts and Session
  Status, including partial reports and available fallback-provider allowances.
- Use themed choice menus in account forms, model settings and reasoning controls.
  Add model search and explicit provider/model loading states.
- Keep repository and status dialogs stable across content changes; preserve parent
  surfaces and keyboard focus when nested menus are selected or dismissed.

## 0.7.7

- Unify Status, repository search, composer menus, approvals and shared-workspace
  surfaces with the compact settings design. Show the selected account in Status.
- Open GitHub search immediately with a spinner, recoverable inline errors and
  retry. Keep cached results visible and respect closing during a pending fetch.
- Publish account and model changes without rereading the workspace or sending
  conversation history. Recheck entitlements immediately after account switching
  and discard probes invalidated by a switch or settings change.
- Reuse GitHub lists briefly per signed-in session, serve loaded workspaces on
  reopening, and skip unchanged webview storage writes.
- Preserve native input keyboard handling inside menus and restore focus on Escape.
- Update event-listener, chacha20 and spin to compatible patched releases.

## 0.7.6

- Keep settings at the same size across sections, loading and error states.
  Scroll content inside the dialog and keep its footer visible in short windows.
- Give conversation search, change review and other dialogs stable dimensions
  so filtering and expanding content do not move the surrounding surface.

## 0.7.5

- Replace general loading tiles and elapsed timers with a compact Radix spinner;
  keep the tile animation for streaming messages. Show enabled toggles in green.
- Coalesce concurrent workspace refreshes and publish accounts and policy before
  slow model probes finish. Keep existing data visible during background checks.
- Reuse detection for 60 seconds and pause readiness polling while unfocused.
  Explicit refresh, account activation and completed sign-in trigger fresh discovery.
- Deliver streaming tokens directly without full workspace reads and batch token
  bursts once per animation frame. Flush live updates before merging history.
- Refresh account states after automatic switching, limits and recovery without
  forcing model probes. Reject attempts to switch to a limited or signed-out account.

## 0.7.4

- Keep chats, repositories and accounts visible through connection failures;
  show tile loaders and skeleton rows until checks finish, with retry on errors.
- Delay settings until saved values arrive instead of flashing defaults.
- Use compact single-line chat rows without status dots or repeated completion
  dates; tighten search dialogs, menus and settings, and shorten their labels.
- Adapt Beautiful UI file chips and unified diffs into expandable change previews,
  with actual line numbers and additions/deletions. Require a loaded diff to apply.
- Preserve keyboard navigation, focus restoration and Escape dismissal during loading.

## 0.7.3

- Show one account row per authenticated provider/email in both account management
  and the run picker; preserve stored profiles and credentials when reordering.
- Temporarily pause local models and Docker execution, hide their controls and
  settings, stop readiness probes, and disable automatic local fallback.
- Replace the old history popup with a searchable conversation dialog and shared
  sidebar rows. Search titles and original requests, clear filters, navigate with
  arrows/Enter, and open search with Ctrl/Cmd+K.
- Use desktop-style right-click and overflow menus for conversation actions;
  retain edit forms on failures and await deletion acknowledgements.
- Refine conversation spacing, file chips, search fields, empty states and icons.

## 0.7.2

- Replace toast popups with quiet status updates, persistent dismissible errors,
  and a recent-activity menu. Preserve unresolved errors through successful actions.
- Replace tall account cards with compact rows and desktop-style action menus;
  remove green outlines and usage bars, and display readable plans such as Pro Lite.
- Standardize button sizes, field spacing, menu surfaces and close controls across
  settings and the composer. Use the desktop's menu primitive for keyboard navigation,
  placement, outside-click dismissal and focus restoration.
- Keep Settings open when dismissing nested menus and recover cleanly from a failed
  extension message transport.

## 0.7.1

- Keep accounts and models visible during readiness refresh and transient probe
  errors; prevent older background results from overwriting recent account edits.
- Collapse multiple authenticated profiles for the same provider/email into one
  run-picker choice while retaining their individual profiles in account management.
- Render the account picker in a fixed overlay with reliable outside-click and
  keyboard behavior, larger labels, and real provider logos.
- Adapt the desktop's tile loader, shimmer labels, elapsed time, expandable tool
  traces and changed-file chips, including smooth completion disclosures.
- Use the desktop font stack and clearer text sizes, remove blur transitions,
  and keep multiline drafts sized correctly after programmatic edits and resizing.

## 0.7.0

- Bring desktop provider account behavior into the extension: shared CLI sign-ins,
  isolated profiles, active account selection, email/plan identity, session affinity,
  and more reliable authentication and usage-limit recovery.
- Discover the current Claude lineup from the installed CLI and refresh account
  and model readiness while the workbench is open and when VS Code regains focus.
- Redesign the workbench with a responsive session sidebar, search, rename and
  ordering actions, a composer account switcher, clearer states, and theme-native
  account management with provider usage windows.
- Make account changes and settings saves await host acknowledgements; preserve
  concurrent account edits and restore messages after failed submissions.
- Preserve completed replies and streaming text when delayed history arrives,
  and add a jump-to-latest control, accessible dialogs, and reduced-motion styles.
- Port desktop runtime fixes for queues, fallback, budgets, sessions, and worktree
  application while retaining the extension's Docker support.

## 0.6.2

- Make isolated Codex and Claude account sign-in update in place as soon as
  authentication completes, using bounded status checks that never start a
  model turn or consume provider usage.
- Replace the oversized account editor with compact account rows, focused
  connection actions, and collapsed advanced settings.
- Refine account removal, reset-credit confirmation, alerts, responsive layout,
  and provider-error handling with quieter, theme-native UI.
- Preserve provider account state during settings changes so newly added or
  authenticated accounts no longer disappear behind a stale detection cache.
- Temporarily disable and hide Cloud Continuity and same-network collaboration
  at both the UI and runtime boundaries while preserving their implementation.

## 0.6.1

- Stop rejecting provider-native slash-shaped input while keeping interactive
  plugin and MCP setup in the provider's own CLI.
- Add a compact account-scoped **Open CLI** action for managing provider tools
  inside each isolated signed-in profile without copying credentials.
- Simplify Settings and session budgets by removing unsupported integration
  controls, verbose fallback copy, and the oversized usage summary.
- Show reported Codex weekly usage as a small label only when that telemetry is
  available, and hide unsupported budget choices.
- Align slash-command provider badges and rename Continuity to Cloud Continuity
  throughout the UI and documentation.
- Emphasize seamless task continuation across multiple signed-in accounts in
  the README and Marketplace listing.

## 0.5.2

- Fix daemon startup for existing databases when migration files were embedded
  with different Windows and Unix line endings.
- Surface the daemon's actual startup failure instead of only reporting exit
  code 1.
- Replace narrow settings icon tiles with a labeled, scrollable section bar and
  ensure secondary actions remain visible across VS Code themes.
- Simplify account actions, remove detached plus icons, and trim verbose helper
  copy throughout Settings.

## 0.5.1

- Redesign Settings around a calm, spacious navigation and content hierarchy
  with larger icons, cleaner controls, and restrained theme-native surfaces.
- Save account additions, priority, authentication mode, enabled state, and
  reset-credit preferences immediately, so sign-in never depends on a separate
  Apply step.
- Move popover menus into a document-level overlay layer so Recent Sessions and
  other menus cannot fade beneath or be clipped by the workbench.
- Refine account rows, provider readiness, model profiles, confirmation dialogs,
  responsive layouts, and global menu interaction targets.
- Add regression coverage for overlay layering and immediate account setup.

## 0.5.0

- Add an ordered multi-account pool for Codex and Claude subscriptions with
  isolated provider-owned authentication and seamless per-account failover.
- Persist account-specific limit/reset state and resume waiting work as soon as
  any authenticated slot becomes available.
- Store Claude setup tokens in the OS credential vault and keep credentials out
  of policies, project files, transcripts, and debug output.
- Keep earned Codex reset-credit redemption off by default behind a per-account
  confirmation, and disable Claude fast-mode paid usage for pooled runs.
- Add a polished Accounts manager for adding, naming, pausing, authenticating,
  reordering, and removing account slots.
- Add five-account failover simulation, credential isolation, durable reset
  state, wire-protocol, and full regression coverage.

## 0.4.0

- Add the current Claude Fable 5.1, Claude Opus 5, and GPT-6 Astra families,
  including model-specific reasoning effort choices.
- Keep model discovery current through the installed Codex app-server catalog,
  Claude CLI/config discovery, latest-tracking Claude aliases, and resilient
  built-in fallbacks when a CLI cannot be queried.
- Add independent Claude and Codex model/reasoning profiles for new runs,
  automatic fallback switches, and exact switch-back restoration.
- Replace the long settings form with a polished, responsive sectioned settings
  experience and refine the workbench's surfaces, depth, and spacing.
- Make the collaboration overwrite test portable across Windows and POSIX line
  endings.

## 0.3.0

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
