# Publishing Perpetual for VS Code

Perpetual ships as a **platform-specific extension**: the daemon is a compiled
Rust binary, so each of the six targets gets its own VSIX containing exactly one
`am-daemon`. The Marketplace serves each user the build for their platform.

Releases are cut by CI. Publishing by hand is the fallback.

## One-time Marketplace setup

CI publishes with **Microsoft Entra ID**, not a Personal Access Token. GitHub
mints a short-lived OIDC token for the release job, Entra trades it for an
access token, and this repository stores no long-lived publishing secret.
(Microsoft retires PATs on 2026-12-01 regardless.)

These steps cannot be done from the CLI.

1. **Create an Azure DevOps organization**, which the Marketplace authenticates
   against: https://dev.azure.com
2. **Create the publisher** at https://marketplace.visualstudio.com/manage. The
   publisher ID is permanent and must match `publisher` in `package.json`
   (currently `SakethSripada`). If the ID is taken, pick another and update
   `package.json` before releasing.
3. **Register an application** in Entra ID (Azure portal → *Microsoft Entra ID* →
   *App registrations* → *New registration*). Note its **Application (client) ID**
   and **Directory (tenant) ID**. No client secret is needed — that is the point.
4. **Add a federated credential** to that app (*Certificates & secrets* →
   *Federated credentials* → *GitHub Actions deploying Azure resources*):

   | Field | Value |
   | --- | --- |
   | Organization | `SakethSripada` |
   | Repository | `Perpetual` |
   | Entity type | **Environment** |
   | Environment name | `marketplace` |

   This binds publishing to the `marketplace` environment of this repository. A
   fork, a branch, or any other workflow cannot obtain a token.
5. **Authorize the identity on the publisher**: Marketplace publisher page →
   *Members* → add the app registration with the **Contributor** role.
6. **Create the `marketplace` environment** in GitHub (*Settings* →
   *Environments*), add a **required reviewer** so releases need approval, and
   set two environment **variables** (not secrets — they are identifiers):

   - `AZURE_CLIENT_ID` — the app's Application (client) ID
   - `AZURE_TENANT_ID` — the Directory (tenant) ID


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

# vsce reads the Azure CLI's credentials, so sign in as an identity that is a
# member of the publisher.
az login
npx vsce publish --azure-credential --packagePath perpetual-vscode-darwin-arm64-<version>.vsix
```

Entra publishing needs `vsce >= 2.26.1`; this repo pins `^3.6.0`. Until
2026-12-01 a PAT scoped to **Marketplace → Manage** with **All accessible
organizations** still works as a fallback, via `npx vsce login <publisher>`.

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
