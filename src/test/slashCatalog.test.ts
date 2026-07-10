import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const appSource = readFileSync(
  path.resolve(__dirname, "../../webview/src/App.tsx"),
  "utf8",
);

test("slash picker exposes only Perpetual-owned commands", () => {
  assert.match(appSource, /const APP_COMMANDS: AppCommand\[\] = \[/);
  assert.match(appSource, /name: "plan"/);
  assert.match(appSource, /name: "review"/);
  assert.match(appSource, /name: "model"/);
  assert.match(appSource, /name: "permission"/);
  assert.match(appSource, /return APP_COMMANDS\.filter/);
});

test("slash commands resolve to settings or read-only structured runs", () => {
  assert.match(appSource, /function resolveAppCommand/);
  assert.match(appSource, /kind: "read_only_run"/);
  assert.match(appSource, /permission: "read_only"/);
  assert.match(appSource, /function planPrompt/);
  assert.match(appSource, /function reviewPrompt/);
  assert.match(appSource, /function permissionFromCommand/);
});

test("unknown slash commands are rejected instead of passed to a CLI", () => {
  assert.match(appSource, /is not supported in Perpetual/);
  assert.match(appSource, /const message = run\?\.message \?\? text/);
  assert.doesNotMatch(appSource, /isNativeSlashCommandText/);
});

test("the picker completes only the leading command token", () => {
  assert.match(appSource, /const slashStart = value\.match\(\/\^\\s\*\//);
  assert.match(appSource, /applySlashCompletion\(draft, slashState, slashMatches\[0\]\)/);
});
