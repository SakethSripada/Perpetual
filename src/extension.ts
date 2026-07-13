import * as vscode from "vscode";
import { DaemonManager } from "./node/daemonManager";
import { WorkbenchController } from "./node/workbenchController";
import { WorkbenchWebviewProvider } from "./node/webviewProvider";

const VIEW_ID = "perpetual.workbench";
const PANEL_VIEW_ID = "perpetual.workbench.panel";
let activeDaemon: DaemonManager | null = null;

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Perpetual");
  const daemon = new DaemonManager(context, output);
  activeDaemon = daemon;
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
    // Same provider also backs the panel container so Perpetual can live in the
    // bottom panel (beside Terminal) or be dragged to the secondary sidebar.
    vscode.window.registerWebviewViewProvider(PANEL_VIEW_ID, provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand("perpetual.openWorkbench", () => {
      provider.openPanel();
      void controller.refresh();
    }),
    vscode.commands.registerCommand("perpetual.newSession", () => controller.newSession()),
    vscode.commands.registerCommand("perpetual.refresh", () => controller.refresh()),
    vscode.commands.registerCommand("perpetual.connectLocalRepo", () =>
      controller.connectLocalRepoInteractive()
    ),
    vscode.commands.registerCommand("perpetual.connectGithubRepo", () =>
      controller.connectGithubRepoInteractive()
    ),
    vscode.commands.registerCommand("perpetual.openSettings", () =>
      vscode.commands.executeCommand("workbench.action.openSettings", "@ext:SakethSripada.perpetual-for-vscode")
    )
  );
}

export async function deactivate(): Promise<void> {
  await activeDaemon?.prepareShutdown();
  activeDaemon = null;
}
