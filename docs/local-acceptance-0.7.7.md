# Local acceptance — 0.7.7

Validated on Windows, October 8, 2026. Work remains on `dev`; no push or release.
The desktop reference repository was not modified.

## Completed checks

- Extension unit suite: 93 passed, including account switching, invalidation
  during model discovery, progressive resource updates, GitHub session cache
  isolation, retry failures, submission recovery and streaming reconciliation.
- Rust workspace: 298 passed. Five integration tests are ignored by default.
- Extension and webview type checks and production packaging passed.
- VS Code activation smoke test passed. Installed extension, webview and daemon
  hashes match the built artifacts.
- Live Codex model discovery passed and returned the current provider catalogue.
- Production npm audit: zero vulnerabilities.
- Rust audit: passed without warnings after compatible updates to event-listener,
  chacha20 and spin. The existing audit script verifies the disabled SQLx RSA
  dependency is absent before applying its documented advisory exception.
- Browser fixture checks covered Status, settings, account actions, model search,
  permissions, budgets, repositories and keyboard dismissal. GitHub failure,
  retry, empty results and closing during a pending request were exercised.
  Light and dark themes, narrow panels and short windows were checked.

## Live provider results

The real approval integration suite ran all four tests in disposable repositories,
asking each provider to run only `git --version`.

- Claude Ask: passed; approval surfaced and command succeeded.
- Claude WorkspaceWrite: passed; approval surfaced and command succeeded.
- Codex Ask: failed after approval; its Windows sandbox could not start the shell.
- Codex WorkspaceWrite: failed; the same sandbox setup failure prevented execution.

The Codex sandbox log identified an OS error 32 sharing violation while validating
the active Codex browser runtime's `node_repl.exe`. Its setup error was
`helper_unknown_error: setup refresh had errors`. The matching upstream report is
https://github.com/openai/codex/issues/51601.

This provider environment failure remains a launch blocker. No sandbox bypass,
system ACL changes or unrelated process termination was performed. Re-run the
Codex live tests after the provider runtime issue is resolved. Browser fixtures
exercise the extension UI but do not establish that every live sign-in/provider
combination works.

## Performance changes verified

- Detection completion publishes a small resource patch, with zero workspace
  reads or transcript transfer; token streaming still avoids workspace snapshots.
- Cached discovery is reused for 60 seconds. Switching invalidates model
  entitlements immediately; a stale in-flight result cannot mark them fresh.
- Loaded workspace state appears immediately when another webview opens.
- Unchanged composer state does not cause a webview storage write.
- GitHub lists are cached for two minutes per signed-in session. Failures are not
  cached; closing the picker during a request does not reopen it on completion.
- Existing refresh coalescing, focus-aware polling and frame-batched streaming
  remain enabled. Local models, Docker, cloud continuity and LAN collaboration
  remain temporarily unavailable.
