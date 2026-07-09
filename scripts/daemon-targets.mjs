import path from "node:path";

export const TARGETS = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "win32-x64": "x86_64-pc-windows-msvc",
  "win32-arm64": "aarch64-pc-windows-msvc",
};

export function readArg(args, name) {
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === name) return args[i + 1];
    if (arg.startsWith(`${name}=`)) return arg.slice(name.length + 1);
  }
  return undefined;
}

export function currentTarget() {
  const { platform, arch } = process;
  if (platform === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  if (platform === "win32") return arch === "arm64" ? "win32-arm64" : "win32-x64";
  if (platform === "linux") return arch === "arm64" ? "linux-arm64" : "linux-x64";
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

export function targetTriple(target) {
  const triple = TARGETS[target];
  if (!triple) {
    throw new Error(`Unknown target: ${target}. Known targets: ${Object.keys(TARGETS).join(", ")}`);
  }
  return triple;
}

export function binaryName(target) {
  return target.startsWith("win32") ? "am-daemon.exe" : "am-daemon";
}

export function releaseBinaryPath(root, target) {
  return path.join(root, "target", targetTriple(target), "release", binaryName(target));
}

export function hostReleaseBinaryPath(root, target = currentTarget()) {
  return path.join(root, "target", "release", binaryName(target));
}
