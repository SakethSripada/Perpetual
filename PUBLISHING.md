# Publishing Perpetual for VS Code

Perpetual ships as a **platform-specific extension**: the daemon is a compiled
Rust binary, so each of the six targets gets its own VSIX containing exactly one
`am-daemon`. The Marketplace serves each user the build for their platform.

Releases are cut by CI. Publishing by hand is the fallback.

## One-time Marketplace setup

These steps cannot be done from the CLI.

1. **Create an Azure DevOps organization** (the Marketplace authenticates against
   it): https://dev.azure.com
2. **Create a Personal Access Token** scoped to **Marketplace → Manage**, with
   the organization set to **All accessible organizations**. A token scoped to a
   single org fails to publish.
3. **Create the publisher** at https://marketplace.visualstudio.com/manage. The
   publisher ID is permanent and must match `publisher` in `package.json`
   (currently `perpetual`). If the ID is taken, pick another and update
   `package.json` before releasing.
4. **Store the token** as the `VSCE_PAT` secret in the GitHub repository, inside
   an environment named `marketplace`. Add a required reviewer to that
   environment so a release needs a human approval.

> **PATs are retired on 2026-12-01.** After that, publishing uses Microsoft Entra
> ID: federate an identity with this repository and swap the publish step to
> `npx vsce publish --azure-credential` with `id-token: write` permission. No
> stored secret is needed then, which is the better setup for a public repo.

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
4. waits for approval on the `marketplace` environment,
5. publishes every target under one version, and
6. attaches the VSIXes to a GitHub release.

## Publishing by hand

Only when CI is unavailable. Every target must be built on a host that can
target it; never substitute a binary built for another OS or architecture.

```sh
npm ci
npm run build:daemon -- --target=darwin-arm64
npm run copy-daemon -- --target=darwin-arm64
npm run package:darwin-arm64

npx vsce login <publisher>
npx vsce publish --packagePath perpetual-vscode-darwin-arm64-<version>.vsix
```

## Verifying a VSIX

```sh
unzip -l perpetual-vscode-darwin-arm64-<version>.vsix
```

It must contain `dist/extension.js`, the webview bundle, `media/`, and exactly
one `bin/<target>/am-daemon`. It must not contain source maps, `node_modules/`,
raw `src/`, `webview/src/`, `crates/`, `target/`, or any local data or tokens.

## Marketplace asset rules

- The `icon` in `package.json` must be a PNG of at least 128×128. It cannot be an
  SVG. (`media/activity.svg` is a view-container icon inside the extension, which
  is unaffected by this rule.)
- README and CHANGELOG images cannot be SVGs, and must resolve over HTTPS. Relative
  links are rewritten by `vsce` against the `repository` URL on the default
  branch, so referenced images must be committed there.
- Badges, if added, must come from a trusted provider.
- At most 30 keywords.

## References

- Publishing: https://code.visualstudio.com/api/working-with-extensions/publishing-extension
- Platform-specific extensions: https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions
- Webview security: https://code.visualstudio.com/api/extension-guides/webview
- Workspace Trust: https://code.visualstudio.com/api/extension-guides/workspace-trust
