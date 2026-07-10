import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const appSource = readFileSync(
  path.resolve(__dirname, "../../webview/src/App.tsx"),
  "utf8",
);
const stylesSource = readFileSync(
  path.resolve(__dirname, "../../webview/src/styles.css"),
  "utf8",
);
const registrySource = appSource.slice(
  appSource.indexOf("const APP_COMMANDS"),
  appSource.indexOf("function availableSlashCommands"),
);

test("slash picker exposes exactly the commands Perpetual owns", () => {
  const names = [...registrySource.matchAll(/\bname: "([^"]+)"/g)].map(
    (match) => match[1],
  );
  assert.deepEqual(names, [
    "plan",
    "review",
    "model",
    "permissions",
    "effort",
    "init",
    "security-review",
    "debug",
    "simplify",
    "run",
    "verify",
    "diff",
    "status",
    "new",
    "resume",
    "stop",
    "settings",
    "help",
  ]);
  assert.doesNotMatch(registrySource, /name: "compact"/);
  assert.doesNotMatch(registrySource, /name: "mcp"/);
  assert.doesNotMatch(registrySource, /name: "logout"/);
});

test("slash commands resolve to app actions, settings, or structured runs", () => {
  assert.match(appSource, /function resolveAppCommand/);
  assert.match(appSource, /kind: "local"/);
  assert.match(appSource, /kind: "setting"/);
  assert.match(appSource, /kind: "run"/);
  assert.match(appSource, /permission: "read_only"/);
  assert.match(appSource, /requiresWrite: true/);
  assert.match(appSource, /function securityReviewPrompt/);
  assert.match(appSource, /function verifyPrompt/);
  assert.match(appSource, /function permissionFromCommand/);
});

test("unknown slash commands are rejected instead of passed to a CLI", () => {
  assert.match(appSource, /is not supported in Perpetual/);
  assert.match(appSource, /const message = run\?\.message \?\? text/);
  assert.doesNotMatch(appSource, /isNativeSlashCommandText/);
});

test("the picker completes only the leading command token", () => {
  assert.match(appSource, /const slashStart = value\.match\(\/\^\\s\*\//);
  assert.match(
    appSource,
    /applySlashCompletion\(draft, slashState, slashMatches\[0\]\)/,
  );
});

test("the empty repository picker remains neutral", () => {
  assert.match(appSource, /className="composer-icon-btn"/);
  assert.doesNotMatch(stylesSource, /\.composer-icon-btn\.warning/);
});
