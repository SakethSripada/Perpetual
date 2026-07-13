# Publishing Perpetual for VS Code

Perpetual ships as a **platform-specific extension**: the daemon is a compiled
Rust binary, so each of the six targets gets its own VSIX containing exactly one
`am-daemon`. The Marketplace serves each user the build for their platform.

Releases are cut by GitHub Actions. Publishing by hand is the fallback.

## One-time Marketplace setup

1. Create the publisher at https://marketplace.visualstudio.com/manage. The
   publisher ID must match `publisher` in `package.json` (currently
   `SakethSripada`).
2. In Azure DevOps, open your profile menu → **Personal access tokens** →
   **New Token**. Choose **All accessible organizations**, then
   **Custom defined → Marketplace → Manage**.
3. In the GitHub repository, add the token as an Actions secret named
   `VSCE_PAT` under **Settings → Secrets and variables → Actions**.

This uses a Marketplace PAT for the current setup. It does not require an Azure
subscription, resource group, managed identity, service connection, or
federated credential.

## Cutting a release

```sh
# 1. Bump the version and land it on main.
npm version 0.2.0 --no-git-tag-version
# 2. Update CHANGELOG.md, commit, and push.
# 3. Tag. The tag must match package.json or CI fails the release.
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/release.yml` then:

1. verifies the tag matches `package.json`,
2. builds all six daemons from `crates/` on hosts that can target them,
3. packages one VSIX per target (`check-daemon` blocks a stale or foreign binary),
4. attaches the VSIXes to a GitHub release, and
5. publishes all six VSIXes to the Marketplace using `VSCE_PAT`.

## Publishing by hand

Only when CI is unavailable. Every target must be built on a host that can
target it; never substitute a binary built for another OS or architecture.

```powershell
npm ci
npm run build:daemon -- --target=win32-x64
npm run copy-daemon -- --target=win32-x64
npm run package:win32-x64

$env:VSCE_PAT = "<paste locally; never commit or send this value>"
npx @vscode/vsce publish --packagePath perpetual-for-vscode-win32-x64-<version>.vsix
Remove-Item Env:VSCE_PAT
```

## Verifying a VSIX

```sh
unzip -l perpetual-for-vscode-win32-x64-<version>.vsix
```

It must contain `dist/extension.js`, the webview bundle, `media/`, and exactly
one `bin/<target>/am-daemon`. It must not contain source maps, `node_modules/`,
raw `src/`, `webview/src/`, `crates/`, `target/`, or any local data or tokens.

## Marketplace asset rules

- The `icon` in `package.json` must be a PNG of at least 128x128. It cannot be an
  SVG. (`media/activity.svg` is a view-container icon inside the extension,
  which is unaffected by this rule.)
- README and CHANGELOG images cannot be SVGs, and must resolve over HTTPS.
  Relative links are rewritten by `vsce` against the `repository` URL on the
  default branch, so referenced images must be committed there.
- Badges, if added, must come from a trusted provider.
- At most 30 keywords.

## References

- Publishing: https://code.visualstudio.com/api/working-with-extensions/publishing-extension
- Continuous integration: https://code.visualstudio.com/api/working-with-extensions/continuous-integration
- Platform-specific extensions: https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions
- Webview security: https://code.visualstudio.com/api/extension-guides/webview
- Workspace Trust: https://code.visualstudio.com/api/extension-guides/workspace-trust
