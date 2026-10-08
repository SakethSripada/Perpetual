# Desktop parity: extension 0.7.0

## 0.7.1 follow-up

Readiness invalidation now retains successful account/model state. Failed probes
retain those values, and an account revision guard prevents an older background
probe from overwriting an acknowledged edit. The switcher groups authenticated
profiles by provider/email without deleting credentials or changing session affinity.
It uses a fixed portal outside surrounding clipping/stacking contexts.

The desktop's `ai.tsx` loader, shimmer, elapsed timer, collapsible tool traces,
and file chips are adapted directly, with their MIT notice included in
`licenses/beautiful-ui.txt`. Typography uses the desktop system font stack and
larger menu labels. Composer drafts resize after state updates and panel changes.

Verification: 75 extension tests, TypeScript/webview checks, extension-host
activation, and browser checks at wide and 391px widths in dark/light themes.
The fixture includes duplicate identities and `?refresh` sends snapshots every
second to exercise popup stability. Real provider prompts are not required.

Reference: sibling `Perpetual-Desktop`, inspected without edits. Changes are local
on `dev`; packaging and installation do not publish a release.

## Gaps addressed

| Area | Extension changes |
| --- | --- |
| Provider identity and selection | Shared system sign-ins and isolated profiles; installed, authenticated, active, paused and limited states; email and plan; explicit account activation and session account affinity. |
| Account usability | Composer switcher and grouped provider menu; account management, profile creation, login/token flows, rename/order/enable/remove actions, reported provider usage and reset times. |
| Model discovery | Desktop Claude binary discovery, existing Codex app-server discovery, readiness refresh on a timer and window focus. |
| Runtime | Desktop fixes across fallback/retry, queues, budgets, auth checks, CLI sessions, streaming and worktree application. |
| Navigation | Wide session sidebar, search, rename, ordering, review/stop/delete actions; compact history retained for narrow VS Code surfaces. |
| Feel and accessibility | VS Code theme tokens, quieter spacing and typography, responsive accounts/composer/settings, dialog focus trapping, keyboard account navigation and reduced motion. |
| Data integrity | Serialized account mutations use current policy; general settings preserve accounts; acknowledged saves retain forms on errors; failed sends restore drafts; delayed history cannot rewind streamed replies. |

The extension retains its Docker execution support. Existing disabled Cloud
Continuity/LAN behavior remains disabled. Desktop-only Tauri/window controls and
its unused workflow-settings migration are outside the extension adaptation.

## Verification

- Rust workspace: 298 tests passed; five intentionally ignored live tests.
- TypeScript/webview: 70 tests passed and both projects typecheck.
- VS Code extension host: activation and all public commands verified.
- Packaged native daemon: real authenticated wire client, fresh database,
  create/rename/reorder, account status, model catalog and restart persistence.
- Browser UI: dark/light, wide and 360px panels, settings/account layout,
  welcome prompts, failed-send draft recovery, session rename and dialog keyboard behavior.

`npm run preview:webview` opens a local fixture with fake accounts. It never runs
a provider turn. `?theme=light` checks light mode; `?fail` simulates token-save
failure. `node scripts/smoke-daemon.mjs` requires compiled TypeScript and a copied
native binary, and uses disposable storage.

## 0.7.2 surface refinement

- Reused the desktop's Radix dropdown primitive for account selection, profile
  actions, session actions and activity history; verified menu Escape returns focus
  without closing the surrounding Settings sheet.
- Account rows use shared neutral surfaces, compact spacing, ghost action buttons
  and readable plan labels. Usage summaries show remaining capacity without colored
  progress lines. Settings fields, profile groups, buttons and composer menus share
  a compact control style with visible keyboard focus and reduced-motion support.
- Removed the old toast component and styling. Successful actions use a quiet
  temporary status; unresolved failures remain dismissible and appear in bounded,
  deduplicated recent activity. Transport failures reject requests immediately.
- Verification: 78 extension tests, TypeScript checks, local browser checks of
  nested menus, model/options surfaces, plan labels and failed-send draft recovery;
  dark/light 391px panels, production packaging and the VS Code activation test.

## User acceptance checks

1. Open **Perpetual: Open Panel** in VS Code and check both panel and sidebar sizes.
2. Verify existing system sign-ins, then add a second isolated account. Complete
   the real browser login and check identity, plan, active state and CLI launch.
3. Switch accounts/providers and run a task. Check follow-up session affinity,
   approvals, stop/resume and review/apply on a disposable repository.
4. Verify the installed CLIs' models and reasoning choices; install/update a CLI
   and refocus VS Code to check refresh.
5. Check real usage-limit rotation and recovery when a provider reaches its limit.

Real browser login, paid provider turns and actual usage exhaustion remain user
acceptance checks; the automated tests do not consume provider credits.
