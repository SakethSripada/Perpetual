import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { build } from "esbuild";

async function loadModule(): Promise<any> {
  const dir = mkdtempSync(path.join(tmpdir(), "perpetual-questions-"));
  const outfile = path.join(dir, "questions.cjs");
  await build({
    entryPoints: [path.resolve(__dirname, "../../webview/src/userQuestions.ts")],
    outfile,
    bundle: true,
    platform: "node",
    format: "cjs",
    logLevel: "silent",
  });
  const mod = await import(pathToFileURL(outfile).href);
  rmSync(dir, { recursive: true, force: true });
  return mod;
}

test("normalizes Claude AskUserQuestion options", async () => {
  const { questionsFromEvent } = await loadModule();
  const questions = questionsFromEvent({
    id: "e1",
    kind: "tool_use",
    text: "AskUserQuestion",
    data: {
      input: {
        questions: [{
          header: "Scope",
          question: "Which scope?",
          multiSelect: false,
          options: [
            { label: "Focused", description: "Only this feature" },
            { label: "Broad", description: "Related workflows too" },
          ],
        }],
      },
    },
  });
  assert.deepEqual(questions, [{
    id: "e1-0",
    header: "Scope",
    question: "Which scope?",
    multiSelect: false,
    options: [
      { label: "Focused", description: "Only this feature" },
      { label: "Broad", description: "Related workflows too" },
    ],
  }]);
});

test("normalizes Codex request_user_input ids and answer payload", async () => {
  const { formatQuestionAnswers, questionsFromEvent } = await loadModule();
  const questions = questionsFromEvent({
    id: "e2",
    kind: "tool_use",
    text: "request_user_input",
    data: {
      input: {
        questions: [{
          id: "scope",
          header: "Scope",
          question: "How much?",
          options: [{ label: "Focused", description: "Small" }],
        }],
      },
    },
  });
  assert.equal(questions[0].id, "scope");
  assert.equal(formatQuestionAnswers(questions, { scope: ["Focused"] }), "Focused");
});
