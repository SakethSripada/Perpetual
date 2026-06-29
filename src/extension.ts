import * as vscode from "vscode";
import { DaemonManager } from "./node/daemonManager";
import { WorkbenchController } from "./node/workbenchController";
import { WorkbenchWebviewProvider } from "./node/webviewProvider";

const VIEW_ID = "agentmanager.workbench";
const PANEL_VIEW_ID = "agentmanager.workbench.panel";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("AgentManager");
  const daemon = new DaemonManager(context, output);
  const controller = new WorkbenchController(context, daemon, output);
  const provider = new WorkbenchWebviewProvider(context, controller);

  context.subscriptions.push(
    output,
    daemon,
    controller,
    provider,
    vscode.window.registerWebviewViewProvider(VIEW_ID, provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    // Same provider also backs the panel container so AgentManager can live in the
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
    vscode.commands.registerCommand("agentmanager.openSettings", () =>
      vscode.commands.executeCommand("workbench.action.openSettings", "@ext:agentmanager.agentmanager-vscode")
    )
  );
}

export function deactivate(): void {
  // VS Code disposes context subscriptions automatically.
}
