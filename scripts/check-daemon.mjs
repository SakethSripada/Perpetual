import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

// Verifies that the prebuilt am-daemon binary for a target is present before
// packaging. The binary is produced in the AgentManager monorepo and synced
// into bin/<target>/ via `npm run sync:daemon` there (or `npm run copy-daemon`
// here with AM_DAEMON_BINARY set). See README "Daemon workflow".

const argTarget = process.argv.find((arg) => arg.startsWith("--target="));
const target = argTarget?.split("=")[1] ?? currentTarget();
const binary = target.startsWith("win32") ? "am-daemon.exe" : "am-daemon";
const expected = path.resolve("bin", target, binary);

try {
  const info = await stat(expected);
  if (!info.isFile() || info.size === 0) throw new Error("empty");
  const binaryData = await readFile(expected);
  const missing = requiredDaemonMarkers().filter((marker) => !binaryData.includes(Buffer.from(marker)));
  if (missing.length) {
    console.error(`Daemon binary for ${target} is missing required capabilities: ${expected}`);
    console.error("");
    for (const marker of missing) {
      console.error(`  - ${marker}`);
    }
    console.error("");
    console.error("Sync a daemon build that includes local model fallback and diff support:");
    console.error(`  npm run sync:daemon -- --target=${target} --extension-repo="${process.cwd()}"`);
    process.exit(1);
  }
  console.log(`Daemon present for ${target}: ${expected}`);
  console.log("Daemon capabilities present: local model fallback, cloud continuity, transcript transition markers, thread/work-node diffs.");
} catch {
  console.error(`Missing daemon binary for ${target}: ${expected}`);
  console.error("");
  console.error("This binary is built in the AgentManager monorepo (the Rust crates).");
  console.error("From the monorepo run:");
  console.error(`  npm run sync:daemon -- --target=${target} --extension-repo="${process.cwd()}"`);
  console.error("Then commit the updated bin/ here and retry packaging.");
  process.exit(1);
}

function requiredDaemonMarkers() {
  return [
    "LocalModelPolicy",
    "LocalModelTarget",
    "thread.local_fallback_started",
    "local_fallback_active",
    "echo_user_message",
    "thread.fallback_started",
    "thread.fallback_waiting",
    "thread.switchback_completed",
    "thread.cloud_handoff_started",
    "thread.cloud_reclaimed",
    "ListCloudRuns",
    "LaunchCloudHandoff",
    "ReclaimCloudRun",
    "ollama",
    "lm_studio",
    "ThreadDiff",
    "WorkNodeDiff",
    "/diff",
  ];
}

function currentTarget() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  if (platform === "win32") return arch === "arm64" ? "win32-arm64" : "win32-x64";
  if (platform === "linux") return arch === "arm64" ? "linux-arm64" : "linux-x64";
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}
