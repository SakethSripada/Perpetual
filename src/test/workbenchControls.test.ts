import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
  reasoningOptions(
    agent: "claude_code" | "codex",
    snapshot: any,
    model: string,
  ): { value: string; label: string }[];
  reasoningAfterModelChange(
    agent: "claude_code" | "codex",
    snapshot: any,
    model: string,
    current: string,
  ): string;
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
    [
      "claude-fable-5",
      "claude-opus-4-8",
      "claude-opus-4-7",
      "claude-sonnet-5",
      "claude-sonnet-4-6",
      "claude-haiku-4-5",
    ],
  );
  assert.deepEqual(
    modelOptions("codex", null, null, "").map((option) => option.value),
    [
      "gpt-5.6-sol",
      "gpt-5.6-terra",
      "gpt-5.6-luna",
      "gpt-5.5",
      "gpt-5.4",
      "gpt-5.4-mini",
    ],
  );
});

test("a catalog model with no effort support only offers Default", async () => {
  const { reasoningOptions } = await loadWorkbenchControls();
  const snapshot = {
    modelCatalog: [
      {
        agent: "claude_code",
        reasoning: ["low", "medium", "high", "xhigh", "max"],
        models: [
          {
            id: "claude-haiku-4-5",
            aliases: ["haiku"],
            reasoning: [],
            default_reasoning: null,
          },
          {
            id: "claude-opus-4-8",
            aliases: ["opus"],
            reasoning: ["low", "medium", "high", "xhigh", "max"],
            default_reasoning: null,
          },
        ],
      },
    ],
    runDefaults: [],
  };
  assert.deepEqual(
    reasoningOptions("claude_code", snapshot, "claude-haiku-4-5").map(
      (option) => option.value,
    ),
    [""],
  );
  // Alias selection resolves to the same catalog entry.
  assert.deepEqual(
    reasoningOptions("claude_code", snapshot, "opus").map(
      (option) => option.value,
    ),
    ["", "low", "medium", "high", "xhigh", "max"],
  );
  // Unknown custom ids keep the catalog-wide list.
  assert.deepEqual(
    reasoningOptions("claude_code", snapshot, "claude-mystery-9").map(
      (option) => option.value,
    ),
    ["", "low", "medium", "high", "xhigh", "max"],
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

test("reasoning choices follow the selected model's discovered capabilities", async () => {
  const { reasoningOptions, reasoningAfterModelChange } =
    await loadWorkbenchControls();
  const snapshot = {
    modelCatalog: [
      {
        agent: "codex",
        reasoning: ["low", "medium", "high", "xhigh", "max", "ultra"],
        models: [
          {
            id: "gpt-5.6-sol",
            aliases: [],
            reasoning: ["low", "medium", "high", "xhigh", "max", "ultra"],
            default_reasoning: "medium",
          },
          {
            id: "gpt-5.6-luna",
            aliases: [],
            reasoning: ["low", "medium", "high", "xhigh", "max"],
            default_reasoning: "medium",
          },
        ],
      },
    ],
    runDefaults: [],
  };

  assert.deepEqual(
    reasoningOptions("codex", snapshot, "gpt-5.6-sol").map(
      (option) => option.value,
    ),
    ["", "low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.deepEqual(
    reasoningOptions("codex", snapshot, "gpt-5.6-luna").map(
      (option) => option.value,
    ),
    ["", "low", "medium", "high", "xhigh", "max"],
  );
  assert.equal(
    reasoningAfterModelChange("codex", snapshot, "gpt-5.6-luna", "ultra"),
    "medium",
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

type SignInModule = {
  signInCommand(
    agent: "claude_code" | "codex",
    binary: string,
    shell: string | undefined,
  ): string;
  isPowerShell(shell: string | undefined): boolean;
};

let signInPromise: Promise<SignInModule> | null = null;

/** Bundles the controller against a stub `vscode` so it can load outside the host. */
function loadSignIn(): Promise<SignInModule> {
  if (signInPromise) return signInPromise;
  signInPromise = (async () => {
    const dir = mkdtempSync(path.join(tmpdir(), "perpetual-signin-"));
    const stub = path.join(dir, "vscode.js");
    writeFileSync(stub, "module.exports = { env: {}, window: {}, workspace: {} };");
    const outfile = path.join(dir, "controller.cjs");
    await build({
      entryPoints: [path.resolve(__dirname, "../../src/node/workbenchController.ts")],
      outfile,
      bundle: true,
      platform: "node",
      format: "cjs",
      alias: { vscode: stub },
      logLevel: "silent",
    });
    const loaded = require(outfile) as SignInModule;
    rmSync(dir, { recursive: true, force: true });
    return loaded;
  })();
  return signInPromise;
}

test("PowerShell sign-in invokes the CLI through the call operator", async () => {
  const { signInCommand } = await loadSignIn();
  const binary = "C:\\Users\\dev\\AppData\\Roaming\\npm\\codex.cmd";

  // A bare quoted path is a string literal in PowerShell, so `codex login`
  // parsed as an expression and failed on the `login` token.
  assert.equal(
    signInCommand("codex", binary, "C:\\Windows\\System32\\powershell.exe"),
    `& 'C:\\Users\\dev\\AppData\\Roaming\\npm\\codex.cmd' login`,
  );

  // Windows PowerShell 5.1 has no `||`.
  const claude = signInCommand("claude_code", binary, "pwsh.exe");
  assert.doesNotMatch(claude, /\|\|/);
  assert.match(claude, /\$LASTEXITCODE -ne 0/);
});

test("POSIX shells keep the plain quoted invocation", async () => {
  const { signInCommand, isPowerShell } = await loadSignIn();

  // cmd and git bash both accept double quotes; only the call operator is wrong there.
  const quote = process.platform === "win32" ? '"' : "'";
  assert.equal(
    signInCommand("codex", "/usr/local/bin/codex", "/bin/zsh"),
    `${quote}/usr/local/bin/codex${quote} login`,
  );
  assert.match(
    signInCommand("claude_code", "/usr/local/bin/claude", "/bin/bash"),
    /auth login \|\| /,
  );

  // git bash on Windows must not be mistaken for PowerShell.
  assert.equal(isPowerShell("C:\\Program Files\\Git\\bin\\bash.exe"), false);
  assert.equal(isPowerShell("C:\\Program Files\\PowerShell\\7\\pwsh.exe"), true);
  assert.equal(isPowerShell(undefined), false);
});

test("the visible VS Code workspace is restored as the default repository", () => {
  const controller = readFileSync(
    path.resolve(__dirname, "../../src/node/workbenchController.ts"),
    "utf8",
  );
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  assert.match(controller, /vscode\.workspace\.workspaceFolders/);
  assert.match(controller, /defaultRepoIds = pickDefaultRepoIds\(repos\)/);
  assert.match(app, /const repoTouchedRef = useRef\(false\)/);
  assert.match(app, /const defaultRepoIds = snapshot\?\.defaultRepoIds \?\? \[\]/);
  assert.match(app, /setRepoIds\(defaultRepoIds\)/);
});
