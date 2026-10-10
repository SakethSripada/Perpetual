/** Build an isolated VS Code screenshot host. Never packaged or published. */
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build as viteBuild } from 'vite';
import { build as esbuild } from 'esbuild';
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
if (!process.argv[2]) throw new Error('Pass an output directory outside the repository.');
const target = path.resolve(process.argv[2]);
if (target === repo || target.startsWith(repo + path.sep)) throw new Error('Use a directory outside the repository.');
await fs.mkdir(target, { recursive: true });
const manifest = JSON.parse(await fs.readFile(path.join(repo, 'package.json'), 'utf8'));
manifest.activationEvents = ['onStartupFinished'];
await fs.writeFile(path.join(target, 'package.json'), JSON.stringify(manifest, null, 2));
await fs.cp(path.join(repo, 'media'), path.join(target, 'media'), { recursive: true });
await viteBuild({ configFile: path.join(repo, 'webview/vite.config.ts'), build: { outDir: path.join(target, 'dist/webview') } });
await esbuild({
  entryPoints: [path.join(repo, 'webview/preview.ts')],
  bundle: true, platform: 'browser', format: 'esm', target: 'es2022', jsx: 'automatic',
  loader: { '.css': 'empty' }, outfile: path.join(target, 'dist/webview/assets/index.js'),
  plugins: [{ name: 'marketplace-fixture', setup(build) {
    build.onLoad({ filter: /(?:preview\.ts|App\.tsx)$/ }, async ({ path: filename }) => ({
      contents: (await fs.readFile(filename, 'utf8')).replace('new URLSearchParams(location.search)', 'new URLSearchParams("?marketplace")').replaceAll('acquireVsCodeApi', '__marketplaceVsCodeApi').replace('await import("./src/main");', 'await import("./src/main"); window.__marketplaceVsCodeApi().postMessage({type: "selectThread", threadId: "session-one"});'),
      loader: filename.endsWith('.tsx') ? 'tsx' : 'ts',
    }));
  } }],
});
await esbuild({
  stdin: { contents: `
    import * as vscode from 'vscode';
    import { WorkbenchWebviewProvider } from './src/node/webviewProvider';
    export async function activate(context) {
      const idle = () => ({ dispose() {} });
      const controller = { onSnapshot: idle, onDetectionUpdate: idle, onThreadEvent: idle, onSubmissionFailure: idle, handleMessage() {} };
      const provider = new WorkbenchWebviewProvider(context, controller);
      context.subscriptions.push(provider,
        vscode.window.registerWebviewViewProvider('perpetual.workbench', provider),
        vscode.window.registerWebviewViewProvider('perpetual.workbench.panel', provider),
        vscode.commands.registerCommand('perpetual.openWorkbench', () => provider.openPanel()));
      const project = vscode.workspace.workspaceFolders?.[0]?.uri ?? vscode.Uri.joinPath(context.extensionUri, '..', 'acme-web');
      const source = vscode.Uri.joinPath(project, 'src', 'orders', 'export.ts');
      await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(source), { viewColumn: vscode.ViewColumn.One });
      provider.openPanel();
    }
  `, resolveDir: repo, loader: 'ts' },
  bundle: true, platform: 'node', format: 'cjs', external: ['vscode'], outfile: path.join(target, 'dist/extension.js'),
});
// VS Code's native acquireVsCodeApi property is read-only. The capture build
// substitutes its own bridge before App initializes; production assets are untouched.
console.log(`Screenshot host ready: ${target}\nLaunch VS Code with --extensionDevelopmentPath=${target} --disable-extensions and an isolated --user-data-dir.`);
