import { spawnSync } from "node:child_process";
import process from "node:process";
import { currentTarget, readArg, targetTriple } from "./daemon-targets.mjs";

const args = process.argv.slice(2);
const target = readArg(args, "--target") ?? currentTarget();
const triple = targetTriple(target);

console.log(`Building am-daemon for ${target} (${triple})`);
const result = spawnSync(
  "cargo",
  ["build", "-p", "am-daemon", "--release", "--target", triple],
  { cwd: process.cwd(), stdio: "inherit" }
);

if (result.status !== 0) {
  console.error("");
  console.error(`cargo build failed for ${target} (${triple}).`);
  if (target !== currentTarget()) {
    console.error(`If this Rust target is missing, install it with: rustup target add ${triple}`);
  }
  process.exit(result.status ?? 1);
}
