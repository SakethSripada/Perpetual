import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const appSource = readFileSync(
  path.resolve(__dirname, "../../webview/src/App.tsx"),
  "utf8",
);

test("slash menu shows all filtered native commands instead of a capped subset", () => {
  assert.match(appSource, /slashState && slashMatches\.length > 0/);
  assert.match(appSource, /slashMatches\.map\(\(command\) =>/);
  assert.doesNotMatch(appSource, /slashMatches\.slice\(/);
});

test("slash submit path passes native commands through instead of emulating them", () => {
  assert.doesNotMatch(appSource, /runSlashCommand/);
  assert.doesNotMatch(appSource, /parseSlashSubmit/);
  assert.doesNotMatch(appSource, /promptWithArg/);
  assert.match(appSource, /props\.onSend\(draft\)/);
});

test("slash menu completes the active slash token without rewriting the prompt", () => {
  assert.match(appSource, /parseSlashDraft\(draft, selectionStart\)/);
  assert.match(appSource, /applySlashCompletion\(draft, slashState, slashMatches\[0\]\)/);
  assert.match(appSource, /value\.slice\(0, slashState\.start\)/);
});

test("native skill commands are discoverable for the appropriate agents", () => {
  assert.match(
    appSource,
    /name: "skills",[\s\S]*?aliases: \["skill"\],[\s\S]*?scopes: \["claude_code", "codex"\]/,
  );
  assert.match(
    appSource,
    /name: "reload-skills",[\s\S]*?scopes: \["claude_code"\]/,
  );
  assert.match(
    appSource,
    /name: "run-skill-generator",[\s\S]*?scopes: \["claude_code"\]/,
  );
});

test("slash command catalog remains scoped by selected agent", () => {
  assert.match(appSource, /command\.scopes\.includes\(agent\)/);
  assert.match(appSource, /scopes: \["codex"\]/);
  assert.match(appSource, /scopes: \["claude_code"\]/);
});
