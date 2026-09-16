# Provider capabilities in Perpetual

Perpetual integrates supported agent runtimes; it does not impersonate or
remote-control the ChatGPT, Codex, or Claude desktop applications. This keeps
authentication, billing, plugin authorization, and usage accounting owned by
the provider CLI that the user installed and signed into.

## Integration boundary

| Capability | Codex host sessions | Claude Code host sessions | Notes |
| --- | --- | --- | --- |
| Coding tools and shell | Native | Native | Executed by the selected provider CLI under Perpetual's permission mode. |
| Web search | Native when the selected Codex model/account exposes it | Native when Claude Code exposes it | Tool activity is streamed into the Perpetual transcript. |
| MCP servers | Loaded from the selected Codex profile and project | Loaded from the selected Claude profile and project | Perpetual forwards allow/deny policy instead of reimplementing the servers. |
| Skills and plugins | Loaded by Codex app-server | Loaded by Claude Code | Availability depends on the selected isolated provider profile. |
| Codex apps/connectors | Native through app-server when accessible to the signed-in account | Not applicable | Consequential app calls retain provider-native approval and are routed through Perpetual's approval UI. |
| Image generation/view and subagents | Passed through when app-server emits them | Passed through as Claude tool events | Perpetual renders the normalized lifecycle without owning the implementation. |
| Desktop browser/computer control | Not inherited from the ChatGPT/Codex desktop UI | Not inherited from Claude Desktop/Chrome | Use a provider-supported plugin/MCP integration. Perpetual does not call private desktop IPC or copy browser credentials. |
| API computer use | Not enabled implicitly | Not enabled implicitly | The official APIs require an isolated browser/VM loop owned by the application. That is a separate product surface, not a free pass-through from a consumer subscription. |

Perpetual intentionally does **not** label the last two rows as ready. OpenAI's
built-in browser is a ChatGPT desktop/web surface, and Claude Code computer use
is unavailable to the non-interactive `-p` sessions Perpetual runs. Claude's
`--chrome` integration is also left off until Perpetual has a provider-supported
way to relay every site and action approval in a non-interactive session.

Codex host runs use the documented `codex app-server` protocol, the same rich
client boundary intended for embedded products. Perpetual starts app-server
with the selected `CODEX_HOME`, so authentication, accessible apps, installed
plugins, skills, MCP configuration, workspace policy, and usage stay associated
with that profile. Claude runs use the documented non-interactive Claude Code
CLI stream and its selected `CLAUDE_CONFIG_DIR`.

Provider account slots are intentionally isolated. A plugin or MCP server
installed only in the default profile is not silently copied into another
slot, because its configuration may contain account-specific OAuth state or
secrets. Install or configure the integration in each account slot that should
be allowed to use it.

## Setup in Perpetual

Open **Settings > Integrations**, choose a signed-in account, and select
**Open setup**. Perpetual launches the provider's own interactive CLI with that
account's `CODEX_HOME` or `CLAUDE_CONFIG_DIR` already selected:

- Codex: run `/plugins`; use the Codex MCP controls for MCP servers.
- Claude: run `/plugin`; use `/mcp` for MCP servers.

Installation, OAuth, permissions, and secret storage stay inside the provider.
Closing the terminal without sending a model prompt does not start a Perpetual
task. Newly installed tools are available to new Perpetual sessions for that
same account profile.

## Safety invariants

- Subscription profiles clear ambient API-key overrides before launch so an
  unrelated environment variable cannot silently switch a run to metered API
  billing.
- Tool and MCP policy is passed to both Codex execution transports, including
  app-server; fallback behavior cannot widen the effective tool set.
- Connector actions that expose Accept/Decline choices remain blocked until
  the user answers Perpetual's live approval card.
- Unsupported app-server requests fail closed. Perpetual never returns an empty
  success for token refresh, attestation, or another operation it did not
  perform.
- Arbitrary MCP form/URL elicitations are surfaced and cancelled until the
  workbench has a correlated, schema-validated response UI.
- Provider usage limits and existing task budgets remain authoritative.
  Enabling a tool does not enable credit purchases, extra usage, or automatic
  billing.

## Primary references

- [Codex App Server](https://learn.chatgpt.com/docs/app-server)
- [ChatGPT and Codex plugins](https://learn.chatgpt.com/docs/plugins)
- [ChatGPT built-in browser](https://learn.chatgpt.com/docs/browser)
- [ChatGPT Computer Use](https://learn.chatgpt.com/docs/computer-use)
- [Codex authentication](https://learn.chatgpt.com/docs/auth)
- [OpenAI MCP and connectors](https://developers.openai.com/api/docs/guides/tools-connectors-mcp)
- [OpenAI computer use safety](https://developers.openai.com/api/docs/guides/tools-computer-use)
- [Claude Code CLI reference](https://docs.anthropic.com/en/docs/claude-code/cli-usage)
- [Claude Code plugins](https://code.claude.com/docs/en/plugins)
- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Claude Code computer use](https://code.claude.com/docs/en/computer-use)
- [Claude authentication guidance for third-party tools](https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account)
