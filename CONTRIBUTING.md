# Contributing to Perpetual

Thanks for helping improve Perpetual. Please open an issue before starting a large feature so the direction can be discussed first.

## Development setup

1. Install Node.js 20 or newer, Rust, and the VS Code Extension Development Host requirements.
2. Install the dependencies with `npm ci`.
3. Run `npm test` for the TypeScript unit tests and webview typecheck.
4. Run `cargo test --workspace` for the Rust workspace.
5. Run `npm run build` before packaging or opening a pull request.

The extension launches the daemon binary from `bin/<target>/`. That directory is build output and is not committed, so build your own before running the extension:

```sh
npm run build:daemon -- --target=darwin-arm64   # your platform
npm run copy-daemon -- --target=darwin-arm64
```

Release CI builds all six targets from `crates/` on hosts that can target them. Building a target other than your own additionally needs that target's native linker and C toolchain.

## Pull requests

CI runs `npm test` and the extension build on Linux, macOS, and Windows, plus `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test --workspace`. Run them locally first:

- Keep changes focused and include tests for behavior changes.
- Do not commit generated `bin/`, `dist/`, `out/`, or `target/` directories.
- Do not add credentials, provider tokens, database files, or private workspace paths.
- Run `git diff --check`, `npm test`, `cargo test --workspace`, and `npm run build` before submitting.
- Use a Conventional Commit subject such as `feat(webview): add session history filtering` or `fix(daemon): reject expired client tokens`.

## Reporting security issues

Please do not open a public issue for a suspected vulnerability. Follow the process in [SECURITY.md](SECURITY.md).
