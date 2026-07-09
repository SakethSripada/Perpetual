import assert from "node:assert/strict";
import test from "node:test";
import { expectUnit, responsePayload, variant } from "../node/protocol";

test("serializes unit daemon requests as serde enum variants", () => {
  assert.equal(variant("ping"), "ping");
});

test("serializes payload daemon requests as externally tagged variants", () => {
  assert.deepEqual(variant("list_repos", { project_id: "p1" }), {
    list_repos: { project_id: "p1" },
  });
});

test("extracts typed daemon response payloads", () => {
  const repos = responsePayload<string[]>({ repos: ["a", "b"] }, "repos");
  assert.deepEqual(repos, ["a", "b"]);
});

test("serializes work graph daemon requests", () => {
  assert.deepEqual(variant("move_work_node", {
    node_id: "n1",
    parent_id: null,
    position_x: 12,
    position_y: 24,
  }), {
    move_work_node: {
      node_id: "n1",
      parent_id: null,
      position_x: 12,
      position_y: 24,
    },
  });
});

test("extracts work graph responses", () => {
  const graph = responsePayload<{ project_id: string }>({ work_graph: { project_id: "p1" } }, "work_graph");
  assert.equal(graph.project_id, "p1");
});

test("serializes cloud policy requests", () => {
  assert.equal(variant("get_cloud_policy"), "get_cloud_policy");
  const policy = {
    enabled: true,
    continue_on_sleep: true,
    continue_on_shutdown: false,
    allow_cross_provider: false,
    provider_priority: ["claude_code", "codex"],
    checkpoint_interval_secs: 120,
    monitor_poll_secs: 30,
    stall_timeout_secs: 900,
    max_concurrent_cloud_runs: 2,
    codex_env_id: null,
    require_approval: true,
  };
  assert.deepEqual(variant("set_cloud_policy", policy), { set_cloud_policy: policy });
});

test("serializes local model fallback policy requests", () => {
  assert.equal(variant("get_local_model_policy"), "get_local_model_policy");
  const policy = {
    auto_resume_cloud: true,
    use_local_fallback: true,
    switch_back_to_cloud: true,
    probe_interval_secs: 30,
    offline_grace_secs: 15,
    stable_successes: 2,
    ollama_base_url: "http://127.0.0.1:11434",
    lm_studio_base_url: "http://127.0.0.1:1234",
    lm_studio_api_token_configured: false,
    targets: [{ provider: "ollama", model: "qwen2.5-coder", base_url: "http://127.0.0.1:11434" }],
  };
  assert.deepEqual(variant("set_local_model_policy", policy), { set_local_model_policy: policy });
});

test("extracts cloud policy responses", () => {
  const policy = responsePayload<{ enabled: boolean }>(
    { cloud_policy: { enabled: true } },
    "cloud_policy"
  );
  assert.equal(policy.enabled, true);
});

test("serializes launch-quality workbench requests", () => {
  assert.equal(variant("agent_model_catalog"), "agent_model_catalog");
  assert.equal(variant("detect_local_models"), "detect_local_models");
  assert.deepEqual(variant("list_activity", { project_id: null, limit: 250 }), {
    list_activity: { project_id: null, limit: 250 },
  });
  assert.deepEqual(variant("list_cloud_runs", { thread_id: "t1" }), {
    list_cloud_runs: { thread_id: "t1" },
  });
  assert.deepEqual(variant("launch_cloud_handoff", { thread_id: "t1", agent: "codex" }), {
    launch_cloud_handoff: { thread_id: "t1", agent: "codex" },
  });
  assert.deepEqual(variant("reclaim_cloud_run", { thread_id: "t1" }), {
    reclaim_cloud_run: { thread_id: "t1" },
  });
  assert.deepEqual(variant("apply_thread_changes", { thread_id: "t1" }), {
    apply_thread_changes: { thread_id: "t1" },
  });
  assert.equal(variant("prepare_shutdown"), "prepare_shutdown");
});

test("extracts launch-quality workbench responses", () => {
  assert.deepEqual(
    responsePayload(
      { queued_turns: [{ id: "q1", message: "again", echo_user_message: false }] },
      "queued_turns",
    ),
    [{ id: "q1", message: "again", echo_user_message: false }],
  );
  assert.deepEqual(
    responsePayload({ activity: [{ kind: "thread.fallback_started" }] }, "activity"),
    [{ kind: "thread.fallback_started" }],
  );
  assert.deepEqual(
    responsePayload({ cloud_runs: [{ id: "c1", status: "running" }] }, "cloud_runs"),
    [{ id: "c1", status: "running" }],
  );
  assert.deepEqual(responsePayload({ agent_model_catalogs: [{ agent: "codex" }] }, "agent_model_catalogs"), [
    { agent: "codex" },
  ]);
  assert.deepEqual(responsePayload({ local_model_statuses: [] }, "local_model_statuses"), []);
  assert.deepEqual(
    responsePayload({ local_model_policy: { use_local_fallback: true } }, "local_model_policy"),
    { use_local_fallback: true }
  );
  assert.deepEqual(
    responsePayload({ agent_thread_apply_result: { thread_id: "t1", applied: true } }, "agent_thread_apply_result"),
    { thread_id: "t1", applied: true }
  );
});

test("accepts unit responses only when the daemon returns unit", () => {
  assert.doesNotThrow(() => expectUnit("unit"));
  assert.throws(() => expectUnit("pong"), /expected unit/);
});
