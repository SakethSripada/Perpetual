/// <reference types="mocha" />

import assert from "node:assert/strict";
import * as vscode from "vscode";

suite("Perpetual extension", () => {
  test("activates and registers every public command", async () => {
    const extension = vscode.extensions.getExtension(
      "perpetual.perpetual-vscode",
    );
    assert.ok(extension, "the packaged extension must be discoverable");

    await extension.activate();
    assert.equal(extension.isActive, true);

    const commands = new Set(await vscode.commands.getCommands(true));
    for (const command of [
      "perpetual.openWorkbench",
      "perpetual.newSession",
      "perpetual.refresh",
      "perpetual.connectLocalRepo",
      "perpetual.connectGithubRepo",
      "perpetual.openSettings",
    ]) {
      assert.equal(commands.has(command), true, `${command} is not registered`);
    }
  });
});
