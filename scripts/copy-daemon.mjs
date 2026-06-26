import { chmod, copyFile, mkdir, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const argTarget = process.argv.find((arg) => arg.startsWith("--target="));
const target = argTarget?.split("=")[1] ?? currentTarget();
const binary = target.startsWith("win32") ? "am-daemon.exe" : "am-daemon";
const source =
  process.env.AM_DAEMON_BINARY ??
  path.resolve("..", "target", "release", binary);
const destinationDir = path.resolve("bin", target);
const destination = path.join(destinationDir, binary);

try {
  await stat(source);
} catch {
  console.error(`Missing daemon binary: ${source}`);
  console.error("Run `cargo build -p am-daemon --release` from the repo root first, or set AM_DAEMON_BINARY.");
  process.exit(1);
}

await mkdir(destinationDir, { recursive: true });
await copyFile(source, destination);
if (!target.startsWith("win32")) {
  await chmod(destination, 0o755);
}
console.log(`Copied ${source} -> ${destination}`);

function currentTarget() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  if (platform === "win32") return arch === "arm64" ? "win32-arm64" : "win32-x64";
  if (platform === "linux") return arch === "arm64" ? "linux-arm64" : "linux-x64";
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}
