import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
  CollaborationHost,
  connectCollaborationPeer,
  decodeCollaborationInvite,
  encodeCollaborationInvite,
  type CollaborationInvite,
  type SavedCollaborationPeer,
} from "../node/collaborationTransport";
import { DaemonClient } from "../node/daemonClient";

class MemorySecrets {
  private values = new Map<string, string>();
  get(key: string) {
    return Promise.resolve(this.values.get(key));
  }
  store(key: string, value: string) {
    this.values.set(key, value);
    return Promise.resolve();
  }
  delete(key: string) {
    this.values.delete(key);
    return Promise.resolve();
  }
  onDidChange = () => ({ dispose() {} });
}

class FakeDaemon extends EventEmitter {
  async requestRaw(request: unknown): Promise<any> {
    if (request === "ping") return "pong";
    return { echo: request };
  }
}

function invite(): CollaborationInvite {
  return {
    v: 1,
    hostId: randomUUID(),
    hostName: "desktop",
    addresses: ["127.0.0.1"],
    port: 41234,
    inviteId: randomUUID(),
    secret: Buffer.alloc(32, 7).toString("base64url"),
    expiresAt: Date.now() + 60_000,
  };
}

test("collaboration invites round-trip without exposing raw JSON", () => {
  const original = invite();
  const encoded = encodeCollaborationInvite(original);
  assert.match(encoded, /^perpetual:\/\/join\//);
  assert.equal(encoded.includes(original.secret), false);
  assert.deepEqual(decodeCollaborationInvite(encoded), original);
  assert.throws(() => decodeCollaborationInvite("not-an-invite"), /valid Perpetual device invite/);
});

test("encrypted collaboration proxy supports RPC and per-device reconnect credentials", async () => {
  const secrets = new MemorySecrets();
  const fake = new FakeDaemon();
  const deviceId = randomUUID();
  const hostId = randomUUID();
  const host = new CollaborationHost(
    fake as unknown as DaemonClient,
    secrets as any,
    hostId,
    "Desktop",
  );
  const port = await host.start(0);
  const created = host.createInvite();
  created.invite.addresses = ["127.0.0.1"];
  created.invite.port = port;
  const first = await connectCollaborationPeer(created.invite, deviceId);
  const client = await DaemonClient.connectStream(first.stream, first.credential);
  assert.equal(await client.requestRaw("ping"), "pong");
  await assert.rejects(
    client.requestRaw("prepare_shutdown"),
    /not available through a paired device/,
  );
  await assert.rejects(
    client.requestRaw({
      register_collaboration_device: {
        id: randomUUID(),
        name: "Impersonated device",
      },
    }),
    /identity does not match/,
  );
  client.dispose();
  host.dispose();

  const restarted = new CollaborationHost(
    fake as unknown as DaemonClient,
    secrets as any,
    hostId,
    "Desktop",
  );
  const restartedPort = await restarted.start(0);
  const saved: SavedCollaborationPeer = {
    v: 1,
    hostId,
    hostName: "Desktop",
    addresses: ["127.0.0.1"],
    port: restartedPort,
    inviteId: created.invite.inviteId,
    deviceId,
    credential: first.credential,
  };
  const second = await connectCollaborationPeer(saved, deviceId);
  const reconnected = await DaemonClient.connectStream(second.stream, second.credential);
  assert.equal(await reconnected.requestRaw("ping"), "pong");
  reconnected.dispose();

  await restarted.revokeDevice(deviceId);
  await assert.rejects(
    connectCollaborationPeer(saved, deviceId),
    /Could not reach/,
  );
  restarted.dispose();
});
