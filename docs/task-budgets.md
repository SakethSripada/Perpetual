# Graceful session task budgets

Perpetual task budgets are response-boundary targets for one session. They are
designed to help an agent finish the highest-value work and leave a useful
handoff, not to stop a provider at an exact token. The provider can finish one
in-flight response after the target is crossed.

## Available modes

| Execution path | Token target | Percentage windows |
| --- | --- | --- |
| Claude Code on Host with Claude subscription telemetry | Yes | Rolling 5-hour and/or 7-day |
| Codex on Host with ChatGPT authentication and a reported 7-day window | Yes | 7-day |
| Codex on Host with API-key authentication | Yes | No |
| Docker Sandbox, local models, or cloud handoff | No in v1 | No |

Existing sessions continue with **No limit**. A budget applies to the whole
session, including follow-up turns and hosted provider changes. Token usage
includes provider-reported input, cached-input, and output tokens. Duplicate
and cumulative provider reports are reconciled before they are stored.

## Percentage windows

For Claude, choose either or both of the provider's rolling windows:

- **5-hour** limits how much this session may increase the current rolling
  five-hour usage window.
- **7-day** limits how much this session may increase the current weekly usage
  window.

Claude percentage budgets require the CLI to report the selected subscription
window. Claude.ai subscriber status data can expose these windows after the
first provider response; if a selected window is missing or malformed,
Perpetual fails closed and pauses instead of allowing an unmetered run.

For Codex, the v1 composer exposes the reliably reported 7-day account window.
Codex's provider-owned five-hour limits are not selectable in this mode.

## Weekly percentage meaning

`5%` means “allow this session to increase the account's current 7-day usage
by approximately five percentage points.” It does not mean “stop when the
account reaches five percent.” The account window is provider-level, so other
Codex activity on the same account can make a session stop early.

Codex must report a usable 7-day quota window before a weekly-budgeted prompt
is sent. If that telemetry is missing or malformed, Perpetual fails closed and
pauses the session. A rolling window can decrease as older usage expires; that
does not restore budget already consumed by the session.

## Graceful closeout

Budgeted runs receive private launch guidance to prioritize valuable work and
reserve capacity for validation and a concise status summary. Perpetual sends
one private progress reminder at 50% and begins closeout with 15% remaining.
For token targets, the closeout reserve is clamped to 4,000–20,000 tokens.
For percentage targets, it is 15% of the selected allocation in the window
that is approaching its cap.

Closeout asks the agent to stop starting substantive work, safely finish the
operation already in flight, run only the highest-value validation that fits,
and report completed work, remaining work, blockers, and workspace state. When
the target is reached, Perpetual pauses the session, suppresses automatic
fallback and queued-turn draining, and does not spend a separate summary turn.
The final response may therefore overshoot the configured target by one
provider response.

Raw usage values, quota windows, and private steering instructions are not
published to the webview or transcript. Users see the configured static cap
and the final work summary.

## Changing a budget

Before the first turn, any validated mode can be selected. Once a turn starts,
the mode cannot be changed or reduced. While a session is stopped, the user
can increase its active cap or turn budgeting off; increasing a cap preserves
the session's prior consumption and allows it to resume.

Token targets accept 10,000 through 10,000,000 tokens. Weekly allocations
accept integer values from 1 through 100 percentage points. The composer uses
static labels such as `50k` and `5%`; it never displays live consumption.

These guarantees are intentionally approximate because provider accounting,
stream timing, and account-level quota activity can vary at response
boundaries.
