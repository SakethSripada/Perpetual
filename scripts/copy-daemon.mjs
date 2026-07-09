import { chmod, copyFile, mkdir, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import {
  binaryName,
  currentTarget,
  hostReleaseBinaryPath,
  readArg,
  releaseBinaryPath,
} from "./daemon-targets.mjs";

const args = process.argv.slice(2);
const target = readArg(args, "--target") ?? currentTarget();
const binary = binaryName(target);
const root = process.cwd();
const source = process.env.AM_DAEMON_BINARY ?? await builtBinary(root, target);
const destinationDir = path.resolve("bin", target);
const destination = path.join(destinationDir, binary);

try {
  await stat(source);
} catch {
  console.error(`Missing daemon binary: ${source}`);
  console.error(`Build it from this extension repo first: npm run build:daemon -- --target=${target}`);
  process.exit(1);
}

await mkdir(destinationDir, { recursive: true });
await copyFile(source, destination);
if (!target.startsWith("win32")) {
  await chmod(destination, 0o755);
}
console.log(`Copied ${source} -> ${destination}`);

async function builtBinary(rootDir, targetName) {
  const targeted = releaseBinaryPath(rootDir, targetName);
  try {
    await stat(targeted);
    return targeted;
  } catch {
    if (targetName === currentTarget()) {
      return hostReleaseBinaryPath(rootDir, targetName);
    }
    return targeted;
  }
}
