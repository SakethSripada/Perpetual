import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { build } from "esbuild";

type ModelOption = { value: string; label: string; source: string };
type WorkbenchControlsModule = {
  providerAccountDisplayName(account: any, status?: any): string;
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
  runDefaults(snapshot: any, agent: "claude_code" | "codex"): {
    model: string | null;
    reasoning: string | null;
  };
  sameStringSet(a: readonly string[], b: readonly string[]): boolean;
  collaborationExecutionTargets(
    snapshot: any,
    agent: "claude_code" | "codex",
    active: any,
  ): { value: string; label: string }[];
  collaborationAssignmentIssue(assignment: any): {
    kind: string;
    title: string;
    message: string;
    missingRepos: string[];
  };
  unresolvedCollaborationIssues(assignments: any[]): any[];
};

let modulePromise: Promise<WorkbenchControlsModule> | null = null;

test("account names use authenticated identity without overwriting custom labels", async () => {
  const { providerAccountDisplayName } = await loadWorkbenchControls();
  const account = { agent: "codex", label: "Codex 1" };
  const identity = { authenticated: true, email: "person@example.com" };
  assert.equal(providerAccountDisplayName(account, identity), "person@example.com");
  assert.equal(providerAccountDisplayName({ agent: "claude_code", label: "Claude account 2" }, identity), "person@example.com");
  assert.equal(providerAccountDisplayName({ ...account, label: "Work" }, identity), "Work");
  assert.equal(providerAccountDisplayName(account, { ...identity, authenticated: false }), "Codex 1");
  assert.equal(providerAccountDisplayName(account), "Codex 1");
  assert.equal(providerAccountDisplayName({ ...account, label: "" }), "Codex");
});

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
      "claude-fable-5-1",
      "claude-opus-5",
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
      "gpt-6-astra",
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

test("provider profiles override CLI defaults when switching agents", async () => {
  const { runDefaults } = await loadWorkbenchControls();
  const snapshot = {
    limitPolicy: {
      agent_profiles: [
        { agent: "claude_code", model: "fable", reasoning: "high" },
        { agent: "codex", model: "gpt-6-astra", reasoning: "low" },
      ],
    },
    runDefaults: [
      { kind: "codex", model: "gpt-5.5", reasoning: "medium" },
    ],
  };
  assert.deepEqual(runDefaults(snapshot, "codex"), {
    model: "gpt-6-astra",
    reasoning: "low",
  });
  assert.deepEqual(runDefaults(snapshot, "claude_code"), {
    model: "fable",
    reasoning: "high",
  });
});

test("repo snapshot acknowledgement compares repository sets", async () => {
  const { sameStringSet } = await loadWorkbenchControls();
  assert.equal(sameStringSet(["repo-a", "repo-b"], ["repo-b", "repo-a"]), true);
  assert.equal(sameStringSet(["repo-a"], ["repo-a", "repo-b"]), false);
  assert.equal(sameStringSet([], []), true);
});

test("shared-workspace execution targets route members through their device", async () => {
  const { collaborationExecutionTargets } = await loadWorkbenchControls();
  const now = new Date().toISOString();
  const device = (id: string, name: string) => ({
    id,
    name,
    revoked_at: null,
    last_seen_at: now,
    capabilities: [
      { agent: "codex", installed: true, authenticated: true, version: null },
    ],
  });
  const member = {
    collaboration: {
      connected: true,
      role: "member",
      device_id: "laptop",
      server_time: now,
      devices: [device("desktop", "Desktop"), device("laptop", "Laptop")],
    },
  };
  assert.deepEqual(
    collaborationExecutionTargets(member, "codex", null).map((item) => item.value),
    ["desktop", "laptop"],
  );
  assert.equal(
    collaborationExecutionTargets(member, "codex", null)[1].label,
    "Laptop · this device",
  );

  const host = {
    collaboration: {
      ...member.collaboration,
      role: "host",
      device_id: "desktop",
    },
  };
  assert.deepEqual(
    collaborationExecutionTargets(host, "codex", null).map((item) => item.value),
    ["local", "laptop"],
  );
});

test("collaboration failures stay actionable until a newer attempt exists", async () => {
  const { collaborationAssignmentIssue, unresolvedCollaborationIssues } =
    await loadWorkbenchControls();
  const failed = {
    id: "failed-1",
    thread_id: "thread-1",
    device_id: "laptop",
    device_name: "Laptop",
    agent: "codex",
    status: "failed",
    error:
      "Repository mapping failed on this device. Missing: api, web.",
    created_at: "2026-07-28T10:00:00Z",
  };
  assert.deepEqual(collaborationAssignmentIssue(failed), {
    kind: "missing_repo",
    title: "Repository clone needed",
    message: "Open a matching clone on Laptop, then retry this work.",
    missingRepos: ["api", "web"],
  });
  assert.deepEqual(
    unresolvedCollaborationIssues([failed]).map((assignment) => assignment.id),
    ["failed-1"],
  );
  assert.deepEqual(
    unresolvedCollaborationIssues([
      failed,
      {
        ...failed,
        id: "retry-1",
        status: "queued",
        error: null,
        created_at: "2026-07-28T10:01:00Z",
      },
    ]),
    [],
  );
});

test("ambiguous repository clones are not selected arbitrarily", async () => {
  const { collaborationAssignmentIssue } = await loadWorkbenchControls();
  const issue = collaborationAssignmentIssue({
    device_name: "Laptop",
    agent: "codex",
    status: "failed",
    error:
      "Repository mapping failed on this device. Multiple open clones match: api. Keep only the intended clone open.",
  });
  assert.equal(issue.kind, "ambiguous_repo");
  assert.deepEqual(issue.missingRepos, ["api"]);
  assert.match(issue.message, /keep only the intended clone open/i);
});

test("expired collaboration leases explain fencing and recovery", async () => {
  const { collaborationAssignmentIssue } = await loadWorkbenchControls();
  const issue = collaborationAssignmentIssue({
    device_name: "Desktop",
    agent: "claude_code",
    status: "lease_expired",
    error: "device lease expired",
  });
  assert.equal(issue.kind, "connection");
  assert.match(issue.message, /No late changes were accepted/);
  assert.match(issue.message, /Bring Desktop online and retry/);
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

test("overlay menus escape the clipped workbench stacking context", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  const styles = readFileSync(
    path.resolve(__dirname, "../../webview/src/styles.css"),
    "utf8",
  );
  assert.match(app, /import \{ createPortal \} from "react-dom"/);
  assert.match(app, /createPortal\([\s\S]*document\.body/);
  assert.match(styles, /\.popover \{[\s\S]*z-index: 1000/);
});

test("new provider accounts persist before authentication", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  assert.match(app, /const saveAccounts = \(nextAccounts: ProviderAccount\[\]\)/);
  assert.match(app, /props\.onSaveLimitPolicy\(next\)/);
  assert.doesNotMatch(app, /Apply settings before authenticating/);
  assert.doesNotMatch(app, /disabled=\{!saved\} onClick=\{\(\) => props\.onSignInProviderAccount/);
});

test("settings stay legible and compact at narrow panel widths", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  const styles = readFileSync(
    path.resolve(__dirname, "../../webview/src/styles.css"),
    "utf8",
  );
  assert.doesNotMatch(app, /Private by design/);
  assert.doesNotMatch(app, /Configure how Perpetual runs and hands off work/);
  assert.match(styles, /\.sheet footer button \{[\s\S]*color: var\(--vscode-button-secondaryForeground/);
  assert.doesNotMatch(app, /className="settings-section-picker"/);
  assert.doesNotMatch(styles, /\.settings-nav button span \{ display: none; \}/);
  assert.doesNotMatch(styles, /grid-template-columns: repeat\(6, minmax\(72px, 1fr\)\)/);
  const settingsStyles = readFileSync(
    path.resolve(__dirname, "../../webview/src/settings.css"), "utf8",
  );
  // Inactive categories must stay hidden even when a layout rule sets display.
  for (const section of ["accounts", "agents", "switching", "local", "sandbox"]) {
    assert.ok(app.includes(`hidden={section !== "${section}"}`));
    assert.ok(app.includes(`id="settings-panel-${section}"`));
  }
  assert.match(settingsStyles, /\[data-settings-section\]\[hidden\]\s*\{\s*display: none !important/);
  assert.match(settingsStyles, /@container \(max-width: 470px\)/);
});

test("provider integrations do not add a separate settings surface", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  const controller = readFileSync(
    path.resolve(__dirname, "../../src/node/workbenchController.ts"),
    "utf8",
  );

  assert.doesNotMatch(app, /settings-panel-integrations/);
  assert.doesNotMatch(app, /Provider app only/);
  assert.doesNotMatch(controller, /openProviderAccountSetup/);
  assert.match(app, /Manage plugins and MCP for this account/);
  assert.match(app, /<span>Open CLI<\/span>/);
  assert.match(controller, /client\.providerAccountToolingLaunch\(accountId\)/);
  assert.doesNotMatch(app, /How plugins work|Integration setup|Computer use setup/);
});

test("session budget shows only reported Codex usage in a compact label", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );
  const styles = readFileSync(
    path.resolve(__dirname, "../../webview/src/styles.css"),
    "utf8",
  );

  assert.match(app, /weeklySupported && weeklyWindow && weeklyRemaining !== null/);
  assert.match(app, /className="budget-usage"/);
  assert.doesNotMatch(app, /className="usage-summary"/);
  assert.match(styles, /\.budget-usage \{/);
  assert.doesNotMatch(styles, /\.usage-summary \{/);
});

test("slash command metadata stays aligned across command lengths", () => {
  const styles = readFileSync(
    path.resolve(__dirname, "../../webview/src/styles.css"),
    "utf8",
  );

  assert.match(styles, /\.slash-item \{[\s\S]*grid-template-columns: 120px minmax\(0, 1fr\)/);
  assert.match(styles, /\.slash-item span \{[\s\S]*text-overflow: ellipsis/);
});

test("paused cloud and LAN implementations are runtime-gated", () => {
  const webFlags = readFileSync(
    path.resolve(__dirname, "../../webview/src/featureFlags.ts"), "utf8",
  );
  const nodeFlags = readFileSync(
    path.resolve(__dirname, "../../src/node/featureFlags.ts"), "utf8",
  );
  const controller = readFileSync(
    path.resolve(__dirname, "../../src/node/workbenchController.ts"), "utf8",
  );
  const manager = readFileSync(
    path.resolve(__dirname, "../../src/node/daemonManager.ts"), "utf8",
  );

  assert.match(webFlags, /CLOUD_CONTINUITY_ENABLED = false/);
  assert.match(webFlags, /LAN_COLLABORATION_ENABLED = false/);
  assert.match(nodeFlags, /CLOUD_CONTINUITY_ENABLED = false/);
  assert.match(nodeFlags, /LAN_COLLABORATION_ENABLED = false/);
  assert.match(controller, /CLOUD_MESSAGE_TYPES\.has\(message\.type\)/);
  assert.match(controller, /COLLABORATION_MESSAGE_TYPES\.has\(message\.type\)/);
  assert.match(manager, /if \(!LAN_COLLABORATION_ENABLED\) return local/);
  assert.match(manager, /client\.setCloudPolicy\(\{ \.\.\.policy, enabled: false \}\)/);
});

test("provider authentication refreshes in place without starting model work", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"), "utf8",
  );
  const controller = readFileSync(
    path.resolve(__dirname, "../../src/node/workbenchController.ts"), "utf8",
  );

  assert.match(controller, /watchProviderAuthentication\(accountId, launch\.label, reply\)/);
  assert.match(controller, /client\.providerAccountStatuses\(\)/);
  assert.match(controller, /authPendingAccountIds: \[\.\.\.this\.authPendingAccounts\]/);
  assert.doesNotMatch(controller, /watchProviderAuthentication[\s\S]{0,2500}submitAgentThread/);
  assert.match(app, /authPendingAccountIds\?\.includes\(account\.id\)/);
  assert.match(app, /<summary>Account settings<\/summary>/);
  assert.doesNotMatch(app, /New isolated profile|Token or isolated profile/);
});

test("paused cloud features stay out of settings navigation", () => {
  const app = readFileSync(
    path.resolve(__dirname, "../../webview/src/App.tsx"),
    "utf8",
  );

  assert.doesNotMatch(app, /\["continuity", "Cloud Continuity"/);
  assert.doesNotMatch(app, /className="group-title">Continuity</);
});
