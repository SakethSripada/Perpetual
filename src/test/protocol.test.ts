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

test("accepts unit responses only when the daemon returns unit", () => {
  assert.doesNotThrow(() => expectUnit("unit"));
  assert.throws(() => expectUnit("pong"), /expected unit/);
});
