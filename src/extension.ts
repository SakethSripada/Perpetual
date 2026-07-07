import * as vscode from "vscode";
import { DaemonManager } from "./node/daemonManager";
import { WorkbenchController, type NativeAgentMode } from "./node/workbenchController";
import { WorkbenchWebviewProvider } from "./node/webviewProvider";
import type { AgentKind } from "./node/types";

const VIEW_ID = "agentmanager.workbench";
const PANEL_VIEW_ID = "agentmanager.workbench.panel";
let activeDaemon: DaemonManager | null = null;

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Perpetual");
  const daemon = new DaemonManager(context, output);
  activeDaemon = daemon;
  const controller = new WorkbenchController(context, daemon, output);
  const provider = new WorkbenchWebviewProvider(context, controller);
  const native = (command: string, agent: AgentKind, mode: NativeAgentMode) =>
    vscode.commands.registerCommand(command, () => controller.openNativeAgent(agent, mode));

  context.subscriptions.push(
    output,
    daemon,
    controller,
    provider,
    vscode.window.registerWebviewViewProvider(VIEW_ID, provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    // Same provider also backs the panel container so Perpetual can live in the
    // bottom panel (beside Terminal) or be dragged to the secondary sidebar.
    vscode.window.registerWebviewViewProvider(PANEL_VIEW_ID, provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand("agentmanager.openWorkbench", () => {
      provider.openPanel();
      void controller.refresh();
    }),
    vscode.commands.registerCommand("agentmanager.newSession", () => controller.newSession()),
    vscode.commands.registerCommand("agentmanager.refresh", () => controller.refresh()),
    vscode.commands.registerCommand("agentmanager.connectLocalRepo", () =>
      controller.connectLocalRepoInteractive()
    ),
    vscode.commands.registerCommand("agentmanager.connectGithubRepo", () =>
      controller.connectGithubRepoInteractive()
    ),
    native("agentmanager.openNativeClaude", "claude_code", "open"),
    native("agentmanager.openNativeClaudePlan", "claude_code", "plan"),
    native("agentmanager.openNativeClaudeAuto", "claude_code", "auto"),
    native("agentmanager.openNativeClaudeAcceptEdits", "claude_code", "accept_edits"),
    native("agentmanager.openNativeClaudeAgents", "claude_code", "agents"),
    native("agentmanager.openNativeClaudeMcp", "claude_code", "mcp"),
    native("agentmanager.openNativeClaudePlugins", "claude_code", "plugins"),
    native("agentmanager.openNativeClaudeDiagnostics", "claude_code", "diagnostics"),
    native("agentmanager.resumeNativeClaude", "claude_code", "resume"),
    native("agentmanager.openNativeCodex", "codex", "open"),
    native("agentmanager.resumeNativeCodex", "codex", "resume"),
    native("agentmanager.forkNativeCodex", "codex", "fork"),
    native("agentmanager.openNativeCodexCloud", "codex", "cloud"),
    native("agentmanager.openNativeCodexMcp", "codex", "mcp"),
    native("agentmanager.openNativeCodexPlugins", "codex", "plugins"),
    native("agentmanager.openNativeCodexFeatures", "codex", "features"),
    native("agentmanager.openNativeCodexDiagnostics", "codex", "diagnostics"),
    native("agentmanager.openNativeCodexApp", "codex", "app"),
    vscode.commands.registerCommand("agentmanager.openSettings", () =>
      vscode.commands.executeCommand("workbench.action.openSettings", "@ext:agentmanager.agentmanager-vscode")
    )
  );
}

export async function deactivate(): Promise<void> {
  await activeDaemon?.prepareShutdown();
  activeDaemon = null;
}
