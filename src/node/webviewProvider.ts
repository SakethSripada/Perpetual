import * as fs from "fs";
import * as vscode from "vscode";
import type { WorkbenchController, WebviewReply } from "./workbenchController";

const VIEW_ID = "perpetual.workbench";

export class WorkbenchWebviewProvider implements vscode.WebviewViewProvider, vscode.Disposable {
  private readonly webviews = new Set<vscode.Webview>();
  private readonly disposables: vscode.Disposable[] = [];
  private panel: vscode.WebviewPanel | null = null;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly controller: WorkbenchController
  ) {
    this.disposables.push(
      controller.onSnapshot((snapshot) => this.postAll({ type: "snapshot", snapshot })),
      controller.onThreadEvent((event) => this.postAll({ type: "threadEvent", event }))
    );
  }

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.setupWebview(webviewView.webview);
    webviewView.onDidDispose(() => this.webviews.delete(webviewView.webview));
  }

  openPanel(): void {
    if (this.panel) {
      this.panel.reveal(vscode.ViewColumn.Beside);
      return;
    }

    this.panel = vscode.window.createWebviewPanel(
      VIEW_ID,
      "Perpetual",
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.context.extensionUri, "dist", "webview"),
          vscode.Uri.joinPath(this.context.extensionUri, "media"),
        ],
      }
    );
    this.setupWebview(this.panel.webview);
    this.panel.onDidDispose(() => {
      if (this.panel) this.webviews.delete(this.panel.webview);
      this.panel = null;
    });
  }

  postAll(message: unknown): void {
    for (const webview of this.webviews) {
      void webview.postMessage(message);
    }
  }

  dispose(): void {
    for (const disposable of this.disposables) disposable.dispose();
    this.panel?.dispose();
    this.webviews.clear();
  }

  private setupWebview(webview: vscode.Webview): void {
    webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.context.extensionUri, "dist", "webview"),
        vscode.Uri.joinPath(this.context.extensionUri, "media"),
      ],
    };
    webview.html = this.html(webview);
    this.webviews.add(webview);
    this.disposables.push(
      webview.onDidReceiveMessage((message) => {
        const reply: WebviewReply = (response) => void webview.postMessage(response);
        void this.controller.handleMessage(message, reply);
      })
    );
  }

  private html(webview: vscode.Webview): string {
    const nonce = getNonce();
    const scriptPath = vscode.Uri.joinPath(this.context.extensionUri, "dist", "webview", "assets", "index.js");
    const stylePath = vscode.Uri.joinPath(this.context.extensionUri, "dist", "webview", "assets", "index.css");
    // The bundle filenames are stable, so VS Code's webview cache would keep
    // serving a stale build after a rebuild + reload. Bust the cache by tagging
    // each URL with the file's mtime — it changes only when the bundle changes.
    const scriptUri = webview.asWebviewUri(scriptPath).with({ query: `v=${assetVersion(scriptPath)}` });
    const styleUri = webview.asWebviewUri(stylePath).with({ query: `v=${assetVersion(stylePath)}` });
    const iconUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, "media", "icon.png")
    );
    const csp = [
      "default-src 'none'",
      `img-src ${webview.cspSource} data:`,
      `style-src ${webview.cspSource}`,
      `script-src 'nonce-${nonce}'`,
      `font-src ${webview.cspSource}`,
    ].join("; ");

    return /* html */ `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8">
    <meta http-equiv="Content-Security-Policy" content="${csp}">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <link nonce="${nonce}" rel="stylesheet" href="${styleUri}">
    <title>Perpetual</title>
  </head>
  <body>
    <div id="root" data-icon="${iconUri}"></div>
    <script nonce="${nonce}" type="module" src="${scriptUri}"></script>
  </body>
</html>`;
  }
}

function assetVersion(uri: vscode.Uri): string {
  try {
    return String(Math.floor(fs.statSync(uri.fsPath).mtimeMs));
  } catch {
    return "0";
  }
}

function getNonce(): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let out = "";
  for (let i = 0; i < 32; i++) {
    out += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return out;
}
