/// <reference types="mocha" />

import assert from "node:assert/strict";
import * as vscode from "vscode";

suite("Perpetual extension", () => {
  test("activates and registers every public command", async () => {
    const extension = vscode.extensions.getExtension(
      "agentmanager.agentmanager-vscode",
    );
    assert.ok(extension, "the packaged extension must be discoverable");

    await extension.activate();
    assert.equal(extension.isActive, true);

    const commands = new Set(await vscode.commands.getCommands(true));
    for (const command of [
      "agentmanager.openWorkbench",
      "agentmanager.newSession",
      "agentmanager.refresh",
      "agentmanager.connectLocalRepo",
      "agentmanager.connectGithubRepo",
      "agentmanager.openSettings",
    ]) {
      assert.equal(commands.has(command), true, `${command} is not registered`);
    }
  });
});
