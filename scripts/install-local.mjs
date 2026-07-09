import { spawnSync } from "node:child_process";
import { readdir, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { currentTarget, readArg } from "./daemon-targets.mjs";

const args = process.argv.slice(2);
const root = process.cwd();
const target = readArg(args, "--target") ?? currentTarget();
const editor = readArg(args, "--editor") ?? "code";
const npmCommand = npmInvocation();
const editorCommand = process.platform === "win32" && editor === "code" ? "code.cmd" : editor;
const startedAt = Date.now();

runNpm(["run", "build"]);
runNpm(["run", "build:daemon", "--", `--target=${target}`]);
runNpm(["run", "copy-daemon", "--", `--target=${target}`]);
runNpm(["run", `package:${target}`]);

const vsix = await newestVsix(startedAt);
run(editorCommand, ["--install-extension", vsix, "--force"], { viaCmd: isWindowsCommandScript(editorCommand) });
run(editorCommand, ["--reuse-window", root], { viaCmd: isWindowsCommandScript(editorCommand) });

console.log(`Installed ${path.basename(vsix)} into ${editor}.`);
console.log("If this VS Code window was already open, run Developer: Reload Window once.");

async function newestVsix(startedAtMs) {
  const entries = await readdir(root);
  const candidates = [];
  for (const entry of entries) {
    if (!entry.endsWith(".vsix")) continue;
    const fullPath = path.join(root, entry);
    const info = await stat(fullPath);
    if (info.mtimeMs + 1000 < startedAtMs) continue;
    candidates.push({ fullPath, mtimeMs: info.mtimeMs });
  }
  candidates.sort((a, b) => b.mtimeMs - a.mtimeMs);
  if (!candidates.length) {
    throw new Error("VSIX packaging finished but no new .vsix file was found.");
  }
  return candidates[0].fullPath;
}

function npmInvocation() {
  if (process.env.npm_execpath) {
    return { command: process.execPath, argsPrefix: [process.env.npm_execpath], viaCmd: false };
  }
  if (process.platform === "win32") {
    return { command: "npm.cmd", argsPrefix: [], viaCmd: true };
  }
  return { command: "npm", argsPrefix: [], viaCmd: false };
}

function runNpm(commandArgs) {
  run(npmCommand.command, [...npmCommand.argsPrefix, ...commandArgs], { viaCmd: npmCommand.viaCmd });
}

function run(command, commandArgs, options = {}) {
  const result = options.viaCmd
    ? spawnSync(process.env.ComSpec ?? "cmd.exe", ["/d", "/s", "/c", commandLine(command, commandArgs)], {
        cwd: root,
        stdio: "inherit",
      })
    : spawnSync(command, commandArgs, {
        cwd: root,
        stdio: "inherit",
      });
  if (result.error) {
    console.error(`Failed to run ${command}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function isWindowsCommandScript(command) {
  return process.platform === "win32" && /\.cmd$/i.test(command);
}

function commandLine(command, commandArgs) {
  return [command, ...commandArgs].map(quoteWindowsArg).join(" ");
}

function quoteWindowsArg(arg) {
  const text = String(arg);
  if (text.length === 0) return '""';
  if (!/[\s&()^=;!'+,`~|<>"]/u.test(text)) return text;
  return `"${text.replace(/"/g, '""')}"`;
}
