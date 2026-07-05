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
    checkpoint_interval_secs: 120,
    monitor_poll_secs: 30,
    stall_timeout_secs: 900,
    max_concurrent_cloud_runs: 2,
    codex_env_id: null,
    require_approval: true,
  };
  assert.deepEqual(variant("set_cloud_policy", policy), { set_cloud_policy: policy });
});

test("extracts cloud policy responses", () => {
  const policy = responsePayload<{ enabled: boolean }>(
    { cloud_policy: { enabled: true } },
    "cloud_policy"
  );
  assert.equal(policy.enabled, true);
});

test("accepts unit responses only when the daemon returns unit", () => {
  assert.doesNotThrow(() => expectUnit("unit"));
  assert.throws(() => expectUnit("pong"), /expected unit/);
});
