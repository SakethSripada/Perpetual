import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { build } from "esbuild";

type WorkerExports = {
  mapRepositories(bindings: any[], central: any[], local: any[]): {
    repoIds: string[];
    centralToLocal: Map<string, string>;
    localToCentral: Map<string, string>;
  };
  normalizeRemote(value: string): string;
  sanitizeApprovalForCoordinator(value: any): any;
};

let workerExports: Promise<WorkerExports> | null = null;

async function loadWorkerExports(): Promise<WorkerExports> {
  if (workerExports) return workerExports;
  workerExports = (async () => {
    const dir = mkdtempSync(path.join(tmpdir(), "perpetual-worker-test-"));
    const stub = path.join(dir, "vscode.js");
    writeFileSync(stub, "module.exports = { workspace: { workspaceFolders: [] } };");
    const outfile = path.join(dir, "worker.cjs");
    await build({
      entryPoints: [path.resolve(__dirname, "../../src/node/collaborationWorker.ts")],
      outfile,
      bundle: true,
      platform: "node",
      format: "cjs",
      alias: { vscode: stub },
      logLevel: "silent",
    });
    const loaded = require(outfile) as {
      collaborationWorkerTestExports: WorkerExports;
    };
    rmSync(dir, { recursive: true, force: true });
    return loaded.collaborationWorkerTestExports;
  })();
  return workerExports;
}

test("device workers map differently located clones by normalized Git remote", async () => {
  const { mapRepositories, normalizeRemote } = await loadWorkerExports();
  assert.equal(
    normalizeRemote("git@github.com:Example/Project.git"),
    "https://github.com/example/project",
  );
  const mapped = mapRepositories(
    [{ repo_id: "central-1", repo_name: "Project" }],
    [
      {
        id: "central-1",
        name: "Project",
        remote_url: "git@github.com:Example/Project.git",
      },
    ],
    [
      {
        id: "local-9",
        name: "renamed-folder",
        remote_url: "https://github.com/example/project.git",
      },
    ],
  );
  assert.deepEqual(mapped.repoIds, ["local-9"]);
  assert.deepEqual([...mapped.centralToLocal], [["central-1", "local-9"]]);
  assert.deepEqual([...mapped.localToCentral], [["local-9", "central-1"]]);
});

test("a single unambiguous local clone remains a safe fallback", async () => {
  const { mapRepositories } = await loadWorkerExports();
  const mapped = mapRepositories(
    [{ repo_id: "central", repo_name: "different-name" }],
    [{ id: "central", name: "different-name", remote_url: null }],
    [{ id: "local", name: "local-name", remote_url: null }],
  );
  assert.deepEqual(mapped.repoIds, ["local"]);
});

test("relayed approvals redact credential-shaped command and tool input", async () => {
  const { sanitizeApprovalForCoordinator } = await loadWorkerExports();
  const sanitized = sanitizeApprovalForCoordinator({
    id: "approval",
    command: ["curl", "Authorization=Bearer abc.def", "api_key=super-secret"],
    input: {
      path: "src/main.ts",
      token: "raw-token",
      nested: { password: "raw-password", safe: "visible" },
    },
    reason: "credential=do-not-send",
  });
  assert.doesNotMatch(JSON.stringify(sanitized), /raw-token|raw-password|super-secret|do-not-send/);
  assert.equal(sanitized.input.path, "src/main.ts");
  assert.equal(sanitized.input.nested.safe, "visible");
});
