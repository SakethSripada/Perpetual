import { execFileSync, spawnSync } from "node:child_process";
import { chmod, cp, mkdir, readFile, rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { readArg } from "./daemon-targets.mjs";

const args = process.argv.slice(2);
const noReload = args.includes("--no-reload");
const installDirArg = readArg(args, "--install-dir");
const target = readArg(args, "--target") ?? currentTarget();
const root = process.cwd();
const manifest = JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
const extensionPrefix = `${manifest.publisher}.${manifest.name}-${manifest.version}`;

run("npm", ["run", "build"]);
run("npm", ["run", "check-daemon", "--", `--target=${target}`]);

const installs = installDirArg
  ? [path.resolve(installDirArg)]
  : await findInstalledExtensions(extensionPrefix);

if (!installs.length) {
  console.error(`Could not find an installed ${extensionPrefix} extension folder.`);
  console.error("Install the VSIX once, then rerun this command.");
  process.exit(1);
}

for (const installDir of installs) {
  await syncInstalledExtension(installDir, target);
  console.log(`Synced rebuilt extension -> ${installDir}`);
}

if (noReload) {
  console.log("Skipped VS Code reload (--no-reload).");
} else {
  await reloadEditor();
}

async function syncInstalledExtension(installDir, targetName) {
  await copyDir(path.join(root, "dist"), path.join(installDir, "dist"));
  await copyDir(path.join(root, "media"), path.join(installDir, "media"));
  await cp(path.join(root, "package.json"), path.join(installDir, "package.json"));

  const binary = targetName.startsWith("win32") ? "am-daemon.exe" : "am-daemon";
  const sourceBinary = path.join(root, "bin", targetName, binary);
  const destBinaryDir = path.join(installDir, "bin", targetName);
  const destBinary = path.join(destBinaryDir, binary);
  await mkdir(destBinaryDir, { recursive: true });
  await cp(sourceBinary, destBinary);
  if (!targetName.startsWith("win32")) await chmod(destBinary, 0o755);
}

async function copyDir(source, destination) {
  await rm(destination, { recursive: true, force: true });
  await mkdir(path.dirname(destination), { recursive: true });
  await cp(source, destination, { recursive: true });
}

async function findInstalledExtensions(prefix) {
  const roots = [
    path.join(os.homedir(), ".vscode", "extensions"),
    path.join(os.homedir(), ".vscode-insiders", "extensions"),
    path.join(os.homedir(), ".cursor", "extensions"),
  ];
  const found = [];
  for (const extensionRoot of roots) {
    let entries;
    try {
      entries = await stat(extensionRoot).then(async (info) => (info.isDirectory() ? await listDir(extensionRoot) : []));
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (entry.startsWith(prefix)) found.push(path.join(extensionRoot, entry));
    }
  }
  return found;
}

async function listDir(dir) {
  const { readdir } = await import("node:fs/promises");
  return readdir(dir);
}

async function reloadEditor() {
  if (process.platform !== "darwin") {
    console.log("Auto reload is only wired for macOS. Reload the VS Code window manually.");
    return;
  }

  const runningBundleIds = runningMacBundleIds();
  const candidates = [
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.todesktop.230313mzl4w4u92",
  ];
  const bundleId = candidates.find((candidate) => runningBundleIds.includes(candidate));
  if (!bundleId) {
    console.log("Synced files, but no running VS Code/Cursor app was detected to reload.");
    return;
  }

  const script = `
tell application id "${bundleId}" to activate
delay 0.2
tell application "System Events"
  keystroke "p" using {command down, shift down}
  delay 0.25
  keystroke "Developer: Reload Window"
  delay 0.1
  key code 36
end tell
`;

  try {
    execFileSync("osascript", ["-e", script], { stdio: "ignore" });
    console.log("Requested Developer: Reload Window in the running editor.");
  } catch (err) {
    console.log("Synced files, but macOS blocked automatic editor reload.");
    console.log("Grant automation/accessibility permission or run Developer: Reload Window manually.");
  }
}

function runningMacBundleIds() {
  try {
    const output = execFileSync(
      "osascript",
      ["-e", 'tell application "System Events" to get bundle identifier of every process'],
      { encoding: "utf8" }
    );
    return output
      .split(",")
      .map((part) => part.trim())
      .filter(Boolean);
  } catch {
    return [];
  }
}

function run(command, commandArgs) {
  const result = spawnSync(command, commandArgs, { cwd: root, stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function currentTarget() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  if (platform === "win32") return arch === "arm64" ? "win32-arm64" : "win32-x64";
  if (platform === "linux") return arch === "arm64" ? "linux-arm64" : "linux-x64";
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}
