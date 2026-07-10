import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { build } from "esbuild";

type ModelOption = { value: string; label: string; source: string };
type WorkbenchControlsModule = {
  modelOptions(
    agent: "claude_code" | "codex",
    snapshot: any,
    localProvider: "ollama" | "lm_studio" | null,
    current: string,
  ): ModelOption[];
  sameStringSet(a: readonly string[], b: readonly string[]): boolean;
};

let modulePromise: Promise<WorkbenchControlsModule> | null = null;

function loadWorkbenchControls(): Promise<WorkbenchControlsModule> {
  if (modulePromise) return modulePromise;
  modulePromise = (async () => {
    const dir = mkdtempSync(path.join(tmpdir(), "perpetual-controls-"));
    const outfile = path.join(dir, "controls.cjs");
    await build({
      entryPoints: [path.resolve(__dirname, "../../webview/src/App.tsx")],
      outfile,
      bundle: true,
      platform: "node",
      format: "cjs",
      logLevel: "silent",
    });
    (globalThis as any).window = {
      acquireVsCodeApi: () => ({
        postMessage: () => undefined,
        getState: () => ({}),
        setState: () => undefined,
      }),
    };
    const mod = await import(pathToFileURL(outfile).href);
    rmSync(dir, { recursive: true, force: true });
    return mod as WorkbenchControlsModule;
  })();
  return modulePromise;
}

test("model picker restores cloud fallbacks when CLI detection is empty", async () => {
  const { modelOptions } = await loadWorkbenchControls();
  assert.deepEqual(
    modelOptions("claude_code", null, null, "").map((option) => option.value),
    ["opus", "sonnet", "haiku"],
  );
  assert.deepEqual(
    modelOptions("codex", null, null, "").map((option) => option.value),
    ["gpt-5-codex", "gpt-5", "gpt-4.1", "o3", "o4-mini"],
  );
});

test("detected and local model catalogs remain authoritative", async () => {
  const { modelOptions } = await loadWorkbenchControls();
  const snapshot = {
    modelCatalog: [
      {
        agent: "codex",
        models: [
          {
            id: "gpt-detected",
            label: "Detected",
            source: "codex_debug_models",
            available: true,
            default: true,
          },
        ],
      },
    ],
    localModels: [
      {
        provider: "ollama",
        label: "Ollama",
        models: [{ id: "qwen-coder", name: "Qwen Coder", loaded: true }],
      },
    ],
    runDefaults: [],
  };
  assert.deepEqual(
    modelOptions("codex", snapshot, null, "").map((option) => option.value),
    ["gpt-detected"],
  );
  assert.deepEqual(
    modelOptions("codex", snapshot, "ollama", "").map(
      (option) => option.value,
    ),
    ["qwen-coder"],
  );
});

test("repo snapshot acknowledgement compares repository sets", async () => {
  const { sameStringSet } = await loadWorkbenchControls();
  assert.equal(sameStringSet(["repo-a", "repo-b"], ["repo-b", "repo-a"]), true);
  assert.equal(sameStringSet(["repo-a"], ["repo-a", "repo-b"]), false);
  assert.equal(sameStringSet([], []), true);
});

test("repo assignment UI retains the serialized write and lock guidance", () => {
  const controller = readFileSync(
    path.resolve(__dirname, "../../src/node/workbenchController.ts"),
    "utf8",
  );
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  assert.match(controller, /repoAssignmentsInFlight/);
  assert.match(controller, /drainRepoAssignments/);
  assert.match(controller, /repoAssignmentFailed/);
  assert.match(app, /pendingRepoAssignmentRef/);
  assert.match(app, /Start a new session to use a different set/);
});
