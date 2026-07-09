import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { readArg, TARGETS, targetTriple } from "./daemon-targets.mjs";

const args = process.argv.slice(2);
const target = readArg(args, "--target") ?? args.find((arg) => !arg.startsWith("--"));
if (!target) {
  console.error("Missing target. Usage: node scripts/package-target.mjs <target>");
  process.exit(1);
}
targetTriple(target);

run("node", ["scripts/check-daemon.mjs", `--target=${target}`]);

const tempDir = await mkdtemp(path.join(os.tmpdir(), "perpetual-vscodeignore-"));
const ignoreFile = path.join(tempDir, ".vscodeignore");
try {
  const baseIgnore = await readFile(".vscodeignore", "utf8");
  const ignoredTargetFolders = Object.keys(TARGETS)
    .filter((candidate) => candidate !== target)
    .map((candidate) => `bin/${candidate}/**`)
    .join("\n");
  await writeFile(
    ignoreFile,
    `${baseIgnore.trimEnd()}

# Target-specific daemon packaging. Exclude daemon folders for other VSIX targets.
${ignoredTargetFolders}
`,
  );

  run(process.execPath, [
    vsceEntrypoint(),
    "package",
    "--target",
    target,
    "--ignoreFile",
    ignoreFile,
  ]);
} finally {
  await rm(tempDir, { recursive: true, force: true });
}

function run(command, commandArgs) {
  const result = spawnSync(command, commandArgs, { cwd: process.cwd(), stdio: "inherit" });
  if (result.error) {
    console.error(result.error.message);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function vsceEntrypoint() {
  return path.join(process.cwd(), "node_modules", "@vscode", "vsce", "vsce");
}
