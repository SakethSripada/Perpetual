# Publishing Perpetual for VS Code

Perpetual ships as a **platform-specific extension**: the daemon is a compiled
Rust binary, so each of the six targets gets its own VSIX containing exactly one
`am-daemon`. The Marketplace serves each user the build for their platform.

Releases are cut by CI. Publishing by hand is the fallback.

## One-time Marketplace setup

Marketplace publishing uses **Microsoft Entra ID workload identity federation**
through Azure Pipelines. No client secret or PAT is stored in this repository.

1. **Create the publisher** at https://marketplace.visualstudio.com/manage. The
   publisher ID is permanent and must match `publisher` in `package.json`
   (currently `SakethSripada`).
2. **Create a user-assigned managed identity** in Azure named
   `Perpetual-marketplace-publisher`, in the `perpetual-publishing` resource
   group. Assign it the **Reader** role on that resource group.
3. **Create the Azure DevOps service connection** in project settings:
   - Service type: **Azure Resource Manager**
   - Identity type: **App registration or managed identity (manual)**
   - Credential: **Workload identity federation**
   - Name: `Perpetual-Marketplace-Publisher-Managed`
4. Azure DevOps generates an **Issuer** and **Subject identifier** for the
   service connection. Add those exact values to the managed identity under
   **Settings → Federated credentials → Add credential → Other issuer**. Keep
   the audience as `api://AzureADTokenExchange`.
5. Verify the service connection. The Azure DevOps connection must show that its
   configuration is complete before a pipeline can use it.
6. Run `azure-pipelines.yml` once on `main`. Its **Marketplace identity** stage
   prints the managed identity **resource ID** without publishing. In the
   Marketplace publisher page, open **Members**, add that resource ID, and assign
   the **Contributor** role. Use the resource ID here—not the client ID or object
   ID.

The Azure DevOps service connection authenticates the pipeline to Azure. The
Marketplace publisher membership authorizes that identity to publish extensions;
both pieces are required.

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
3. packages one VSIX per target (`check-daemon` blocks a stale or foreign binary), and
4. attaches the VSIXes to a GitHub release.

`azure-pipelines.yml` performs the same six-target packaging and publishes all
six VSIXes to the Marketplace using the verified managed identity service
connection. Its tag trigger means a pushed `v*` tag automatically starts the
Marketplace release pipeline.

## Publishing by hand

Only when CI is unavailable. Every target must be built on a host that can
target it; never substitute a binary built for another OS or architecture.

```sh
npm ci
npm run build:daemon -- --target=darwin-arm64
npm run copy-daemon -- --target=darwin-arm64
npm run package:darwin-arm64

# vsce reads the Azure CLI's credentials. The managed identity must already
# be a Contributor member of the Marketplace publisher.
az login
npx vsce publish --azure-credential --packagePath perpetual-vscode-darwin-arm64-<version>.vsix
```

Entra publishing needs `vsce >= 2.26.1`; this repo pins `^3.6.0`.

## Verifying a VSIX

```sh
unzip -l perpetual-vscode-darwin-arm64-<version>.vsix
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
- Platform-specific extensions: https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions
- Azure Pipelines workload identity: https://learn.microsoft.com/en-us/azure/devops/pipelines/release/configure-workload-identity?view=azure-devops
- Webview security: https://code.visualstudio.com/api/extension-guides/webview
- Workspace Trust: https://code.visualstudio.com/api/extension-guides/workspace-trust
