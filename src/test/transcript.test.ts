import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { build } from "esbuild";

type BuildTranscriptItems = (input: any) => Array<any>;
type ReconcilePendingMessages = (input: any) => Array<any>;
type TranscriptModule = {
  buildTranscriptItems: BuildTranscriptItems;
  reconcilePendingMessages: ReconcilePendingMessages;
};

async function loadTranscriptModule(): Promise<TranscriptModule> {
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
  return {
    buildTranscriptItems: mod.buildTranscriptItems as BuildTranscriptItems,
    reconcilePendingMessages:
      mod.reconcilePendingMessages as ReconcilePendingMessages,
  };
}

test("transcript builder collapses noisy rate-limit lifecycle rows", async () => {
  const { buildTranscriptItems } = await loadTranscriptModule();
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
  const { buildTranscriptItems } = await loadTranscriptModule();
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

test("pending messages reconcile by stable client id without duplicate text matching", async () => {
  const { reconcilePendingMessages } = await loadTranscriptModule();
  const pending = [
    { id: "cm-hi-1", text: "hi" },
    { id: "cm-hi-2", text: "hi" },
  ];

  const remaining = reconcilePendingMessages({
    pending,
    selectedStatus: "running",
    queued: [],
    events: [
      {
        id: "u1",
        role: "user",
        kind: "user_message",
        text: "hi",
        client_message_id: "cm-hi-1",
        data: {},
        ts: "2026-07-09T00:00:00Z",
      },
    ],
  });

  assert.deepEqual(remaining, [{ id: "cm-hi-2", text: "hi" }]);
});

test("pending messages reconcile queued turns and legacy data ids", async () => {
  const { reconcilePendingMessages } = await loadTranscriptModule();
  const pending = [
    { id: "cm-queued", text: "queued follow-up" },
    { id: "cm-data", text: "persisted event" },
    { id: "cm-next", text: "keep me" },
  ];

  const remaining = reconcilePendingMessages({
    pending,
    selectedStatus: "running",
    queued: [
      {
        id: "q1",
        message: "queued follow-up",
        echo_user_message: true,
        client_message_id: "cm-queued",
      },
    ],
    events: [
      {
        id: "u1",
        role: "user",
        kind: "user_message",
        text: "persisted event",
        data: { client_message_id: "cm-data" },
        ts: "2026-07-09T00:00:00Z",
      },
    ],
  });

  assert.deepEqual(remaining, [{ id: "cm-next", text: "keep me" }]);
});

test("first-turn optimistic bubble stays stable until assistant output begins", async () => {
  const { reconcilePendingMessages } = await loadTranscriptModule();
  const pending = [
    { id: "cm-first", text: "Start this", firstTurn: true },
  ];
  const userEvent = {
    id: "u1",
    role: "user",
    kind: "user_message",
    text: "Start this",
    client_message_id: "cm-first",
    data: {},
    ts: "2026-07-09T00:00:00Z",
  };

  assert.deepEqual(
    reconcilePendingMessages({
      pending,
      selectedStatus: "running",
      queued: [],
      events: [userEvent],
    }),
    pending,
  );
  assert.deepEqual(
    reconcilePendingMessages({
      pending,
      selectedStatus: "running",
      queued: [],
      events: [
        userEvent,
        {
          id: "a1",
          role: "assistant",
          kind: "assistant_text",
          text: "On it",
          data: { streaming: true },
          ts: "2026-07-09T00:00:01Z",
        },
      ],
    }),
    [],
  );
});

test("transcript never renders system envelopes and restores slash command text", async () => {
  const { buildTranscriptItems } = await loadTranscriptModule();
  const items = buildTranscriptItems({
    thread: null,
    activities: [],
    queued: [],
    cloudRuns: [],
    events: [
      {
        id: "u1",
        role: "user",
        kind: "user_message",
        text: "Create an implementation plan for this.\n\nRequest:\nAdd login",
        data: {},
        ts: "2026-07-12T00:00:00Z",
      },
      {
        id: "s1",
        role: "system",
        kind: "provider_prompt",
        text: "secret system prompt",
        data: {},
        ts: "2026-07-12T00:00:01Z",
      },
    ],
  });
  assert.equal(items.length, 1);
  assert.equal(items[0].event.text, "/plan Add login");
});
