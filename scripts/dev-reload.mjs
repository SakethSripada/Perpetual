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
const npmCommand = npmInvocation();

runNpm(["run", "build"]);
runNpm(["run", "build:daemon", "--", `--target=${target}`]);
runNpm(["run", "copy-daemon", "--", `--target=${target}`]);
runNpm(["run", "check-daemon", "--", `--target=${target}`]);

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
  await replaceInstalledDaemon(sourceBinary, destBinary);
  if (!targetName.startsWith("win32")) await chmod(destBinary, 0o755);
}

async function replaceInstalledDaemon(source, destination) {
  const maxAttempts = process.platform === "win32" ? 8 : 1;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    if (process.platform === "win32") stopWindowsProcessAt(destination);
    try {
      await cp(source, destination);
      return;
    } catch (err) {
      if (
        process.platform !== "win32" ||
        !["EBUSY", "EPERM"].includes(err?.code) ||
        attempt === maxAttempts
      ) {
        throw err;
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
}

function stopWindowsProcessAt(executablePath) {
  const target = executablePath.replace(/'/g, "''");
  const script = `
$ProgressPreference = 'SilentlyContinue'
$target = [IO.Path]::GetFullPath('${target}')
Get-Process -Name 'am-daemon' -ErrorAction SilentlyContinue |
  ForEach-Object {
    $candidate = $null
    try { $candidate = $_.Path } catch {}
    if (
      $candidate -and
      [String]::Equals(
        [IO.Path]::GetFullPath($candidate),
        $target,
        [StringComparison]::OrdinalIgnoreCase
      )
    ) {
      Stop-Process -Id $_.Id -Force -ErrorAction Stop
      $_.Id
    }
  }
exit 0
`;
  try {
    const encoded = Buffer.from(script, "utf16le").toString("base64");
    const stopped = execFileSync(
      "powershell.exe",
      ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
      { encoding: "utf8" },
    )
      .trim()
      .split(/\s+/)
      .filter(Boolean);
    if (stopped.length) {
      console.log(`Stopped installed daemon process${stopped.length === 1 ? "" : "es"}: ${stopped.join(", ")}`);
    }
  } catch (err) {
    console.error(`Could not stop the installed daemon at ${executablePath}.`);
    if (err?.message) console.error(err.message);
  }
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

function npmInvocation() {
  if (process.env.npm_execpath) {
    return {
      command: process.execPath,
      argsPrefix: [process.env.npm_execpath],
      viaCmd: false,
    };
  }
  if (process.platform === "win32") {
    return { command: "npm.cmd", argsPrefix: [], viaCmd: true };
  }
  return { command: "npm", argsPrefix: [], viaCmd: false };
}

function runNpm(commandArgs) {
  run(npmCommand.command, [...npmCommand.argsPrefix, ...commandArgs], {
    viaCmd: npmCommand.viaCmd,
  });
}

function run(command, commandArgs, options = {}) {
  const result = options.viaCmd
    ? spawnSync(
        process.env.ComSpec ?? "cmd.exe",
        ["/d", "/s", "/c", commandLine(command, commandArgs)],
        { cwd: root, stdio: "inherit" },
      )
    : spawnSync(command, commandArgs, { cwd: root, stdio: "inherit" });
  if (result.error) {
    console.error(`Failed to run ${command}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status ?? 1);
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

function currentTarget() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  if (platform === "win32") return arch === "arm64" ? "win32-arm64" : "win32-x64";
  if (platform === "linux") return arch === "arm64" ? "linux-arm64" : "linux-x64";
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}
