# Local acceptance — 0.7.8

Validated on Windows, October 8, 2026. Committed on `dev`, without push or release.
Perpetual-Desktop remained unchanged.

## Changes and checks

- Repository selection now uses a searchable, fixed-size dialog. Connection actions
  are separated from chat selection; deselecting never disconnects repositories.
  Disconnecting requires an inline confirmation. Existing-chat assignments stay locked.
- Accounts and Session Status share allowance meters and reset times. Session Status
  includes other providers with reported usage, even when the session provider has no
  report. Active sign-in identity is displayed beside provider-level usage.
- Partial usage, null reports, invalid numbers, out-of-range percentages and invalid
  reset dates are covered by the new usage-data test. Missing reports never become
  a fabricated quota. Cached usage remains visible during background discovery.
- Account forms, model defaults and reasoning use the shared themed menu primitive.
  Model choices are searchable; nested selection and Escape retain their parent surface.
- Repository and usage presentation add no host fetches on opening. Usage rendering
  is memoized and reuses its date formatter; repository search stays local.
- Extension unit tests: **94 passed**. Extension and webview type checks passed.
- Production packaging passed. VS Code activation smoke test: **1 passed**.
- Installed version **0.7.8**; extension, JavaScript, CSS and daemon SHA-256 hashes
  match the built artifacts. Opened the workspace through the VS Code CLI and sent
  the extension's panel URI.

## Visual and interaction review

Browser fixtures exercised account switching and account creation/authentication
choices; Accounts, Providers and Models & limits; repository selection, search,
empty/loading/error states, confirmation and locked assignments; GitHub navigation;
conversation search and context actions; change review; permission choices; session
budgets; run options/model and reasoning choices; notifications; and Session Status.
Weekly-only Codex usage with missing Claude usage is visible in Session Status.

Reviewed dark/light themes and 320×500, 391×760 and 960×800 viewports. Repository
loading, empty and error states retain the same 380px height. Narrow toolbars retain
the readable Repos label. The disabled local/Docker/cloud/LAN surfaces stay gated.
Visual checks use fixtures, while activation and installed-artifact checks use VS Code.

## Launch limitation carried forward

No Rust or provider-execution code changed in this update. The previous 0.7.7
validation passed 298 Rust tests and live Codex model discovery. Claude's live
approval tests passed. Codex's Windows approval runs were blocked by its sandbox
setup refresh, which reported an OS sharing violation against the active Codex
browser runtime. That external execution issue remains unresolved; this UI update
is not evidence that it is fixed. See local-acceptance-0.7.7.md for the diagnosis.
