import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { build } from "esbuild";

type BuildTranscriptItems = (input: any) => Array<any>;

async function loadTranscriptBuilder(): Promise<BuildTranscriptItems> {
  const dir = mkdtempSync(path.join(tmpdir(), "perpetual-transcript-"));
  const outfile = path.join(dir, "transcript.cjs");
  await build({
    entryPoints: [path.resolve(__dirname, "../../webview/src/transcript.ts")],
    outfile,
    bundle: true,
    platform: "node",
    format: "cjs",
    logLevel: "silent",
  });
  const mod = await import(pathToFileURL(outfile).href);
  rmSync(dir, { recursive: true, force: true });
  return mod.buildTranscriptItems as BuildTranscriptItems;
}

test("transcript builder collapses noisy rate-limit lifecycle rows", async () => {
  const buildTranscriptItems = await loadTranscriptBuilder();
  const items = buildTranscriptItems({
    thread: {
      id: "t1",
      preferred_agent: "codex",
      original_agent: "codex",
      fallback_agent: "claude_code",
      limit_reset_at: null,
    },
    activities: [],
    queued: [],
    cloudRuns: [],
    events: [
      {
        id: "u1",
        role: "user",
        kind: "user_message",
        text: "Can you give it a pink theme please.",
        data: {},
        ts: "2026-07-09T00:00:00Z",
      },
      {
        id: "s1",
        role: "system",
        kind: "session_started",
        text: null,
        data: {},
        ts: "2026-07-09T00:00:01Z",
      },
      {
        id: "l1",
        role: "system",
        kind: "usage_limit",
        text: "Usage limit reached",
        data: {},
        ts: "2026-07-09T00:00:02Z",
      },
      {
        id: "e1",
        role: "system",
        kind: "session_ended",
        text: "Interrupted",
        data: { status: "interrupted" },
        ts: "2026-07-09T00:00:03Z",
      },
      {
        id: "u2",
        role: "user",
        kind: "user_message",
        text: "Can you give it a pink theme please.",
        data: {},
        ts: "2026-07-09T00:00:04Z",
      },
    ],
  });

  assert.equal(items.filter((item) => item.type === "event" && item.event.role === "user").length, 1);
  assert.equal(items.some((item) => item.type === "event" && item.event.kind === "session_started"), false);
  assert.equal(items.some((item) => item.type === "event" && item.event.kind === "session_ended"), false);
  assert.equal(items.some((item) => item.type === "event" && item.event.kind === "usage_limit"), false);
  assert.equal(
    items.some(
      (item) =>
        item.type === "transition" &&
        item.text === "Codex rate-limited; switching to Claude",
    ),
    true,
  );
});

test("transcript builder shows queued public turns and hides silent carryover", async () => {
  const buildTranscriptItems = await loadTranscriptBuilder();
  const items = buildTranscriptItems({
    thread: null,
    events: [],
    activities: [],
    cloudRuns: [],
    queued: [
      { id: "q1", message: "visible follow-up", echo_user_message: true },
      { id: "q2", message: "silent carryover", echo_user_message: false },
    ],
  });

  assert.deepEqual(
    items.filter((item) => item.type === "queued").map((item) => item.message),
    ["visible follow-up"],
  );
});
