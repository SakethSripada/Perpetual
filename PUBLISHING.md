# Publishing AgentManager for VS Code

This package is self-contained at VSIX time: the extension host bundle, React
webview bundle, Marketplace docs, assets, and one platform-specific
`am-daemon` binary are included in the generated package.

## Source Checklist

- Update `publisher`, repository URLs, support links, and icon ownership in
  `package.json`.
- Keep GitHub auth inside VS Code's authentication provider; do not store GitHub
  OAuth tokens in AgentManager storage.
- Keep Workspace Trust limited mode enabled because local CLIs, Git, and Docker
  run on the user's machine.
- Keep webview resources restricted to `dist/webview` and `media`, with a strict
  CSP and extension-host message passing only.
- Keep Claude Code Docker disabled until its auth/runtime path is safe.

## Build One Platform

Run these commands from the extension repo on the target platform:

```bash
npm install
npm run build:daemon -- --target=darwin-arm64
npm run copy-daemon -- --target=darwin-arm64
npm run package:darwin-arm64
```

Replace `darwin-arm64` with `darwin-x64`, `linux-x64`, `linux-arm64`,
`win32-x64`, or `win32-arm64`. The daemon is built from the vendored Rust
workspace in this repository, not from the AgentManager app repo. The package
scripts use a target-specific VSCE ignore file so each VSIX contains only the
daemon for its target.

## Verify The VSIX

```bash
npx vsce ls --tree --target darwin-arm64
code --install-extension agentmanager-vscode-darwin-arm64-0.1.0.vsix
```

Confirm the file list contains:

- `dist/extension.js`
- `dist/webview/assets/index.js`
- `dist/webview/assets/index.css`
- `media/icon.png`
- `media/screenshot-workbench.png`
- `bin/<target>/am-daemon` or `bin/<target>/am-daemon.exe` on Windows

Confirm it does not contain:

- source maps
- `node_modules/`
- raw `src/` or `webview/src/`
- raw `crates/` or `target/`
- GitHub OAuth tokens or generated local data

## Publish

Use the final Marketplace publisher ID:

```bash
npx vsce login <publisher>
npx vsce publish --target darwin-arm64
```

For manual upload, package first and upload the generated `.vsix` from the
Visual Studio Marketplace publisher management page.

## References

- VS Code webview API and security:
  https://code.visualstudio.com/api/extension-guides/webview
- VS Code webview UX guidance:
  https://code.visualstudio.com/api/ux-guidelines/webviews
- VS Code view UX guidance:
  https://code.visualstudio.com/api/ux-guidelines/views
- VS Code platform-specific VSIX publishing:
  https://code.visualstudio.com/api/working-with-extensions/publishing-extension
- VS Code authentication API:
  https://code.visualstudio.com/api/references/vscode-api#authentication
