# Local acceptance - 0.7.9

Validated on Windows, October 8, 2026. Incremental commits on `dev`; no push or
release. Perpetual-Desktop was inspected read-only and remains unchanged.

## Model and reasoning controls

The composer combines provider, actual resolved model and reasoning in one
searchable menu. It has no Default model entry or speculative built-in choices.
Each provider retains its own explicit choice in VS Code workspace state, with
webview state supporting legacy preferences and immediate restoration. Selecting
a new model resets reasoning to its advertised default. Unsupported levels never
appear; models with no levels show that directly. Known unavailable models cannot
be submitted. Provider choices in existing chats survive preference updates.

Saving sends a small preference patch without fetching repositories or history.
Failed saves restore the confirmed selection and show an actionable inline error.
The redundant Finished/account subtitle has been removed from the chat header.

## Review correctness and reference behavior

The previous visible-repository implementation compared against the session-start
commit, so later committed edits accumulated. It now compares staged and unstaged
edits against current HEAD, adds real untracked patches and counts, and never
stages files during review. Committing clears those edits; any remaining staged,
unstaged or untracked changes still belong in this whole-repository review.

Isolated worktrees retain their task-base diff, including commits, until applied.
Labels and apply actions distinguish this scope by comparing actual repository
paths rather than relying only on a branch name. Open visible-repository reviews
observe VS Code Git changes with a 250ms debounce and coalesce concurrent reads.
Closed reviews add no Git-triggered diff fetches. Manual refresh remains available.

[Codex review documentation](https://learn.chatgpt.com/docs/code-review) describes
explicit repository scopes, including uncommitted edits, and explains that review
can include edits made outside the agent. [Claude Code interactive documentation](https://code.claude.com/docs/en/interactive-mode)
also distinguishes working-tree, session and branch scopes. This extension labels
its repository and isolated-task scopes explicitly; commit clearing applies to
the uncommitted repository scope. [Claude checkpoints](https://code.claude.com/docs/en/checkpointing)
are separate from Git history and do not justify retaining committed repository
edits in an uncommitted review.

## Validation

- Extension unit tests: 98 passed; extension and webview type checks passed.
- Rust workspace tests: 299 passed, 5 ignored. The new real-Git regression covers
  commit clearing, retained isolated commits, staged and untracked edits, actual
  counts, excluded files, unchanged index bytes and safe UTF-8 truncation.
- Host tests verify independent provider persistence without workspace reads,
  concurrent diff reconciliation, and Git events only refreshing open visible
  reviews. VS Code activation smoke test: 1 passed.
- Browser fixtures exercised model search, per-provider restoration, supported
  and absent reasoning, changes in existing chats, failed-save rollback, loading,
  clean review, isolated review and removed header subtitle. Visual review covered
  dark/light themes and 320x500, 391x760 and 960x800 layouts.
- Production daemon rebuilt for win32-x64; packaged and installed version 0.7.9.
  Installed extension JS, webview JS/CSS and daemon SHA-256 hashes match the build.
  Workspace and Perpetual panel opened through the VS Code CLI.
- `git diff --check` passed. Full Rust formatting check reports pre-existing
  formatting debt outside the changed blocks; no broad reformat was applied.

## Remaining execution limitation

The earlier live Codex Windows approval test was blocked by a sandbox setup
sharing violation against the active Codex browser runtime. This update does not
establish that external issue is fixed. Earlier live Claude approval tests passed;
provider execution was not retested here. See local-acceptance-0.7.7.md.
