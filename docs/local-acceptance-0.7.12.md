# Local acceptance - 0.7.12

October 8, 2026. Model-selector refinement; Perpetual-Desktop unchanged.

## Design

The reasoning panel is 272px wide and approximately 113px tall. It uses neutral
colours, centred effort and model typography, a broad slider rail, and a small
provider menu. The composer pill drops the repeated provider icon. Plain model
rows use a single checkmark; reasoning labels are no longer repeated below the
slider. Search appears only for long model lists. Narrow sidebars preserve the
actual model name while hiding the secondary effort label.

This changes presentation and navigation, preserving catalog-driven reasoning,
per-provider selections, commit-on-release persistence and save-failure rollback.
Opening or navigating the selector introduces no new fetch or storage request.

## Verification

- 97 extension tests passed; extension/webview type checks passed.
- Production win32-x64 package built; VS Code activation test: 1 passed.
- Browser checks: provider switching/restoration, slider keyboard End/reset,
  model search with ArrowDown/Enter, no configurable reasoning, unreported
  defaults, single-level models, initial loading and failed-save rollback.
- Dark 391x760 and light 320x500 inspected. At 320px, page scroll width is 320px
  and the popup remains within the viewport. Escape dismisses and restores focus.
- Installed 0.7.12; extension JS, webview JS/CSS and daemon match build hashes.
  Existing VS Code windows require Developer: Reload Window.
- No Rust/execution changes. This does not establish the external Windows Codex
  sandbox issue recorded in local-acceptance-0.7.7.md as fixed.
