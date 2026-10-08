# Local acceptance - 0.7.11

October 8, 2026. Extension-only design update; Perpetual-Desktop unchanged.

## Design and behavior

The composer uses one compact model/effort pill. Its popover separates model
selection from reasoning adjustment, with provider switching in both views.
The reasoning panel shows a stepped native slider built from the selected
model's reported levels. A reset returns to the reported default; a model change
returns to the reasoning view with that model's default. No model-default entry,
speculative models or unsupported effort levels are introduced.

Long model lists offer search and scroll; short lists have no unnecessary search
field. Models use arrow/Home/End navigation and Enter/Space selection. Escape
returns from the model list to reasoning, then dismisses the panel and restores
focus to the composer trigger. The panel uses [Radix Popover](https://www.radix-ui.com/primitives/docs/components/popover)
for collision placement, dismissal and focus restoration. The effort input keeps
native pointer/keyboard behavior and names its actual level for assistive tools.

Slider previews stay local while dragging or holding a key; persistence commits
on release, with duplicate commits suppressed. Provider preferences and save
failure rollback retain the previously verified host behavior. Opening the panel
adds no fetch or storage call. Unreported defaults show Choose level instead of
claiming a specific level. A single supported level gets one control; models with
no configurable reasoning get a concise explanation. Loading shows its spinner
without a false model list. Names remain readable at 320px, while the larger
composer pill shows both model and effort. Light/dark themes and reduced motion
are supported.

## Validation

- 97 extension tests passed; extension and webview type checks passed.
- Production build and win32-x64 packaging passed; VS Code activation: 1 passed.
- Production dependency audit: zero vulnerabilities reported.
- Browser checks covered both providers, per-provider restoration, mouse and
  keyboard effort changes, reset, search, model keyboard navigation, nested Escape,
  models without reasoning, one-level models, unreported defaults, initial loading,
  save-failure rollback and dark/light themes at 320x500 and 391x760.
- Installed version 0.7.11; extension JS, webview JS/CSS and daemon hashes match
  the built artifacts. Existing VS Code windows require Developer: Reload Window.
- No Rust or execution changes. The external Windows Codex sandbox issue from
  local-acceptance-0.7.7.md is not established as fixed by this UI update.
