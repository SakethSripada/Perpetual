import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { binaryName, currentTarget, readArg } from "./daemon-targets.mjs";

// Verifies that the prebuilt am-daemon binary for a target is present before
// packaging. The binary is built from the vendored Rust workspace in this repo
// and copied into bin/<target>/.

const args = process.argv.slice(2);
const target = readArg(args, "--target") ?? currentTarget();
const binary = binaryName(target);
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
    console.error("Build and copy a fresh daemon from this extension repo:");
    console.error(`  npm run build:daemon -- --target=${target}`);
    console.error(`  npm run copy-daemon -- --target=${target}`);
    process.exit(1);
  }
  console.log(`Daemon present for ${target}: ${expected}`);
  console.log("Daemon capabilities present: local model fallback, cloud continuity, transcript transition markers, thread/work-node diffs.");
} catch {
  console.error(`Missing daemon binary for ${target}: ${expected}`);
  console.error("");
  console.error("Build and copy the daemon from this extension repo:");
  console.error(`  npm run build:daemon -- --target=${target}`);
  console.error(`  npm run copy-daemon -- --target=${target}`);
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
