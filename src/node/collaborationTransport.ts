import {
  createCipheriv,
  createDecipheriv,
  createHmac,
  hkdfSync,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
import { EventEmitter } from "node:events";
import net from "node:net";
import os from "node:os";
import { Duplex } from "node:stream";
import type * as vscode from "vscode";
import type { DaemonClient } from "./daemonClient";
import type { AppEvent } from "./types";

const PROTOCOL_VERSION = 1;
const PROTOCOL_LABEL = "perpetual-collaboration-v1";
const INVITE_PREFIX = "perpetual://join/";
const MAX_WIRE_LINE_BYTES = 24 * 1024 * 1024;
const MAX_CONNECTIONS = 32;
const HANDSHAKE_TIMEOUT_MS = 10_000;
const DEFAULT_INVITE_TTL_MS = 15 * 60 * 1000;
const CREDENTIALS_SECRET_KEY = "perpetual.collaboration.hostCredentials.v1";

export interface CollaborationInvite {
  v: 1;
  hostId: string;
  hostName: string;
  addresses: string[];
  port: number;
  inviteId: string;
  secret: string;
  expiresAt: number;
}

export interface SavedCollaborationPeer {
  v: 1;
  hostId: string;
  hostName: string;
  addresses: string[];
  port: number;
  inviteId: string;
  deviceId: string;
  credential: string;
}

type HostCredential = {
  deviceId: string;
  secret: string;
  createdAt: number;
  lastSeenAt: number;
};

type ClientHello = {
  type: "client_hello";
  v: number;
  inviteId: string;
  deviceId: string;
  nonce: string;
  mac: string;
};

type ServerHello = {
  type: "server_hello";
  v: number;
  nonce: string;
  mac: string;
};

type EncryptedFrame = {
  n: string;
  c: string;
};

type SessionKeys = {
  clientToServer: Buffer;
  serverToClient: Buffer;
};

export class CollaborationHost extends EventEmitter implements vscode.Disposable {
  private server: net.Server | null = null;
  private port = 0;
  private invites = new Map<string, CollaborationInvite>();
  private credentials = new Map<string, HostCredential>();
  private sessions = new Set<HostSession>();
  private disposed = false;

  constructor(
    private readonly localClient: DaemonClient,
    private readonly secrets: vscode.SecretStorage,
    readonly hostId: string,
    readonly hostName: string,
  ) {
    super();
    localClient.on("event", (event: AppEvent) => this.broadcastEvent(event));
  }

  async start(preferredPort = 0): Promise<number> {
    if (this.server) return this.port;
    await this.loadCredentials();
    const server = net.createServer((socket) => void this.accept(socket));
    server.maxConnections = MAX_CONNECTIONS;
    server.on("error", (error) => this.emit("error", error));
    await new Promise<void>((resolve, reject) => {
      const fail = (error: Error) => reject(error);
      server.once("error", fail);
      server.listen({ host: "0.0.0.0", port: preferredPort }, () => {
        server.off("error", fail);
        resolve();
      });
    });
    const address = server.address();
    if (!address || typeof address === "string") {
      server.close();
      throw new Error("Could not determine the collaboration listener port.");
    }
    this.server = server;
    this.port = address.port;
    return this.port;
  }

  createInvite(ttlMs = DEFAULT_INVITE_TTL_MS): { invite: CollaborationInvite; text: string } {
    if (!this.server || !this.port) throw new Error("Collaboration host is not running.");
    this.pruneInvites();
    const invite: CollaborationInvite = {
      v: PROTOCOL_VERSION,
      hostId: this.hostId,
      hostName: this.hostName,
      addresses: lanAddresses(),
      port: this.port,
      inviteId: randomBytes(16).toString("base64url"),
      secret: randomBytes(32).toString("base64url"),
      expiresAt: Date.now() + Math.max(60_000, ttlMs),
    };
    this.invites.set(invite.inviteId, invite);
    return { invite, text: encodeCollaborationInvite(invite) };
  }

  async revokeDevice(deviceId: string): Promise<void> {
    this.credentials.delete(deviceId);
    // A still-live onboarding invite could otherwise let a just-revoked device
    // enroll again. Rotating invitations makes revocation immediate.
    this.invites.clear();
    await this.saveCredentials();
    for (const session of this.sessions) {
      if (session.deviceId === deviceId) session.dispose();
    }
  }

  dispose(): void {
    this.disposed = true;
    for (const session of this.sessions) session.dispose();
    this.sessions.clear();
    this.invites.clear();
    this.server?.close();
    this.server = null;
  }

  private async accept(socket: net.Socket): Promise<void> {
    if (this.disposed || this.sessions.size >= MAX_CONNECTIONS) {
      socket.destroy();
      return;
    }
    socket.setNoDelay(true);
    socket.setTimeout(HANDSHAKE_TIMEOUT_MS, () => socket.destroy());
    try {
      const hello = await readJsonLine<ClientHello>(socket, HANDSHAKE_TIMEOUT_MS);
      if (
        hello.type !== "client_hello" ||
        hello.v !== PROTOCOL_VERSION ||
        !safeIdentifier(hello.deviceId, 128) ||
        !safeIdentifier(hello.inviteId, 128)
      ) {
        throw new Error("invalid collaboration hello");
      }
      const existing = this.credentials.get(hello.deviceId);
      const invite = this.invites.get(hello.inviteId);
      const isNew = !existing;
      const authSecret = existing?.secret ?? invite?.secret;
      if (!authSecret || (isNew && (!invite || invite.expiresAt < Date.now()))) {
        throw new Error("invite expired or device credential revoked");
      }
      const clientNonce = decodeFixed(hello.nonce, 32);
      verifyMac(
        authSecret,
        clientMacPayload(hello.inviteId, hello.deviceId, hello.nonce),
        hello.mac,
      );
      const serverNonce = randomBytes(32);
      const serverNonceText = serverNonce.toString("base64url");
      const serverHello: ServerHello = {
        type: "server_hello",
        v: PROTOCOL_VERSION,
        nonce: serverNonceText,
        mac: mac(
          authSecret,
          serverMacPayload(hello.inviteId, hello.deviceId, hello.nonce, serverNonceText),
        ),
      };
      socket.write(`${JSON.stringify(serverHello)}\n`);
      const keys = deriveSessionKeys(authSecret, clientNonce, serverNonce, hello.deviceId);

      let credential = existing?.secret;
      if (!credential) {
        credential = randomBytes(32).toString("base64url");
        this.credentials.set(hello.deviceId, {
          deviceId: hello.deviceId,
          secret: credential,
          createdAt: Date.now(),
          lastSeenAt: Date.now(),
        });
        await this.saveCredentials();
      } else {
        existing!.lastSeenAt = Date.now();
        void this.saveCredentials();
      }

      const credentialFrame = encryptFrame(
        keys.serverToClient,
        1n,
        "s2c",
        JSON.stringify({ type: "device_credential", credential }),
      );
      socket.write(`${JSON.stringify(credentialFrame)}\n`);
      socket.setTimeout(0);
      const stream = new EncryptedDaemonSocket(
        socket,
        keys.clientToServer,
        keys.serverToClient,
        "c2s",
        "s2c",
        0n,
        1n,
      );
      const session = new HostSession(hello.deviceId, stream, this.localClient, () => {
        this.sessions.delete(session);
      });
      this.sessions.add(session);
      this.emit("deviceConnected", hello.deviceId);
    } catch {
      socket.destroy();
    }
  }

  private broadcastEvent(event: AppEvent): void {
    const frame = JSON.stringify({ event });
    for (const session of this.sessions) session.send(frame);
  }

  private pruneInvites(): void {
    const now = Date.now();
    for (const [id, invite] of this.invites) {
      if (invite.expiresAt < now) this.invites.delete(id);
    }
  }

  private async loadCredentials(): Promise<void> {
    const raw = await this.secrets.get(CREDENTIALS_SECRET_KEY);
    if (!raw) return;
    try {
      const parsed = JSON.parse(raw) as HostCredential[];
      for (const credential of parsed) {
        if (safeIdentifier(credential.deviceId, 128) && credential.secret.length >= 40) {
          this.credentials.set(credential.deviceId, credential);
        }
      }
    } catch {
      await this.secrets.delete(CREDENTIALS_SECRET_KEY);
    }
  }

  private async saveCredentials(): Promise<void> {
    await this.secrets.store(
      CREDENTIALS_SECRET_KEY,
      JSON.stringify([...this.credentials.values()]),
    );
  }
}

class HostSession {
  private buffer = "";
  private authenticated = false;
  private pending = 0;
  private disposed = false;

  constructor(
    readonly deviceId: string,
    private readonly stream: EncryptedDaemonSocket,
    private readonly localClient: DaemonClient,
    private readonly onDispose: () => void,
  ) {
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => this.onData(chunk.toString()));
    stream.on("error", () => this.dispose());
    stream.on("close", () => this.dispose());
    stream.on("end", () => this.dispose());
  }

  send(line: string): void {
    if (this.authenticated && !this.disposed) this.stream.write(`${line}\n`);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stream.destroy();
    this.onDispose();
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    if (this.buffer.length > MAX_WIRE_LINE_BYTES) return this.dispose();
    for (;;) {
      const idx = this.buffer.indexOf("\n");
      if (idx < 0) return;
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (line) void this.onLine(line);
    }
  }

  private async onLine(line: string): Promise<void> {
    if (!this.authenticated) {
      try {
        const handshake = JSON.parse(line) as { token?: string };
        // The outer encrypted handshake is the authentication boundary. The
        // inner token remains present for wire compatibility with DaemonClient.
        if (typeof handshake.token !== "string" || handshake.token.length < 20) {
          throw new Error("invalid inner handshake");
        }
        this.authenticated = true;
        this.stream.write(`${JSON.stringify({ ok: true })}\n`);
      } catch {
        this.dispose();
      }
      return;
    }
    if (this.pending >= 64) {
      this.dispose();
      return;
    }
    let request: { id: number; request: string | Record<string, unknown> };
    try {
      request = JSON.parse(line) as typeof request;
      if (!Number.isSafeInteger(request.id) || request.id < 0 || request.request === undefined) {
        throw new Error("invalid request");
      }
    } catch {
      this.dispose();
      return;
    }
    this.pending += 1;
    try {
      const ok = await this.localClient.requestRaw(request.request);
      this.send(JSON.stringify({ response: { id: request.id, ok } }));
    } catch (error) {
      this.send(
        JSON.stringify({
          response: {
            id: request.id,
            err: error instanceof Error ? error.message : String(error),
          },
        }),
      );
    } finally {
      this.pending -= 1;
    }
  }
}

export async function connectCollaborationPeer(
  peer: CollaborationInvite | SavedCollaborationPeer,
  deviceId: string,
): Promise<{ stream: Duplex; credential: string }> {
  const secret = "credential" in peer ? peer.credential : peer.secret;
  if (!("credential" in peer) && peer.expiresAt < Date.now()) {
    throw new Error("This Perpetual invite has expired. Ask the host for a new one.");
  }
  const hosts = uniqueStrings([
    ...peer.addresses,
    peer.hostName,
    peer.hostName.endsWith(".local") ? peer.hostName : `${peer.hostName}.local`,
  ]);
  const errors: string[] = [];
  for (const host of hosts) {
    try {
      return await connectAddress(host, peer.port, peer.inviteId, secret, deviceId);
    } catch (error) {
      errors.push(`${host}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  throw new Error(`Could not reach ${peer.hostName}. ${errors.slice(0, 3).join("; ")}`);
}

async function connectAddress(
  host: string,
  port: number,
  inviteId: string,
  secret: string,
  deviceId: string,
): Promise<{ stream: Duplex; credential: string }> {
  const socket = net.createConnection({ host, port });
  socket.setNoDelay(true);
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error("connection timed out"));
    }, HANDSHAKE_TIMEOUT_MS);
    socket.once("connect", () => {
      clearTimeout(timer);
      resolve();
    });
    socket.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
  const clientNonce = randomBytes(32);
  const clientNonceText = clientNonce.toString("base64url");
  const hello: ClientHello = {
    type: "client_hello",
    v: PROTOCOL_VERSION,
    inviteId,
    deviceId,
    nonce: clientNonceText,
    mac: mac(secret, clientMacPayload(inviteId, deviceId, clientNonceText)),
  };
  socket.write(`${JSON.stringify(hello)}\n`);
  const serverHello = await readJsonLine<ServerHello>(socket, HANDSHAKE_TIMEOUT_MS);
  if (serverHello.type !== "server_hello" || serverHello.v !== PROTOCOL_VERSION) {
    socket.destroy();
    throw new Error("host returned an incompatible handshake");
  }
  verifyMac(
    secret,
    serverMacPayload(inviteId, deviceId, clientNonceText, serverHello.nonce),
    serverHello.mac,
  );
  const serverNonce = decodeFixed(serverHello.nonce, 32);
  const keys = deriveSessionKeys(secret, clientNonce, serverNonce, deviceId);
  const credentialWire = await readJsonLine<EncryptedFrame>(socket, HANDSHAKE_TIMEOUT_MS);
  const credentialPayload = JSON.parse(
    decryptFrame(keys.serverToClient, 1n, "s2c", credentialWire),
  ) as { type?: string; credential?: string };
  if (
    credentialPayload.type !== "device_credential" ||
    typeof credentialPayload.credential !== "string" ||
    credentialPayload.credential.length < 40
  ) {
    socket.destroy();
    throw new Error("host did not issue a valid device credential");
  }
  return {
    stream: new EncryptedDaemonSocket(
      socket,
      keys.serverToClient,
      keys.clientToServer,
      "s2c",
      "c2s",
      1n,
      0n,
    ),
    credential: credentialPayload.credential,
  };
}

class EncryptedDaemonSocket extends Duplex {
  private incoming = "";
  private plaintext = "";
  private receiveCounter: bigint;
  private sendCounter: bigint;

  constructor(
    private readonly socket: net.Socket,
    private readonly receiveKey: Buffer,
    private readonly sendKey: Buffer,
    private readonly receiveDirection: "c2s" | "s2c",
    private readonly sendDirection: "c2s" | "s2c",
    initialReceiveCounter: bigint,
    initialSendCounter: bigint,
  ) {
    super();
    this.receiveCounter = initialReceiveCounter;
    this.sendCounter = initialSendCounter;
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => this.onWireData(chunk.toString()));
    socket.on("error", (error) => this.destroy(error));
    socket.on("close", () => this.push(null));
    socket.resume();
  }

  override _read(): void {}

  override _write(
    chunk: Buffer | string,
    _encoding: BufferEncoding,
    callback: (error?: Error | null) => void,
  ): void {
    this.plaintext += chunk.toString();
    try {
      for (;;) {
        const idx = this.plaintext.indexOf("\n");
        if (idx < 0) break;
        const line = this.plaintext.slice(0, idx);
        this.plaintext = this.plaintext.slice(idx + 1);
        this.sendCounter += 1n;
        const frame = encryptFrame(
          this.sendKey,
          this.sendCounter,
          this.sendDirection,
          line,
        );
        this.socket.write(`${JSON.stringify(frame)}\n`);
      }
      if (this.plaintext.length > MAX_WIRE_LINE_BYTES) throw new Error("frame is too large");
      callback();
    } catch (error) {
      callback(error instanceof Error ? error : new Error(String(error)));
    }
  }

  override _destroy(error: Error | null, callback: (error?: Error | null) => void): void {
    this.socket.destroy();
    callback(error);
  }

  private onWireData(chunk: string): void {
    this.incoming += chunk;
    if (this.incoming.length > MAX_WIRE_LINE_BYTES * 2) {
      this.destroy(new Error("encrypted frame is too large"));
      return;
    }
    for (;;) {
      const idx = this.incoming.indexOf("\n");
      if (idx < 0) return;
      const line = this.incoming.slice(0, idx).trim();
      this.incoming = this.incoming.slice(idx + 1);
      if (!line) continue;
      try {
        const frame = JSON.parse(line) as EncryptedFrame;
        const expected = this.receiveCounter + 1n;
        const plaintext = decryptFrame(
          this.receiveKey,
          expected,
          this.receiveDirection,
          frame,
        );
        this.receiveCounter = expected;
        this.push(`${plaintext}\n`);
      } catch (error) {
        this.destroy(error instanceof Error ? error : new Error(String(error)));
        return;
      }
    }
  }
}

export function encodeCollaborationInvite(invite: CollaborationInvite): string {
  return `${INVITE_PREFIX}${Buffer.from(JSON.stringify(invite), "utf8").toString("base64url")}`;
}

export function decodeCollaborationInvite(value: string): CollaborationInvite {
  const trimmed = value.trim();
  const encoded = trimmed.startsWith(INVITE_PREFIX)
    ? trimmed.slice(INVITE_PREFIX.length)
    : trimmed;
  let invite: CollaborationInvite;
  try {
    invite = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8")) as CollaborationInvite;
  } catch {
    throw new Error("That is not a valid Perpetual device invite.");
  }
  if (
    invite.v !== PROTOCOL_VERSION ||
    !safeIdentifier(invite.hostId, 128) ||
    !safeIdentifier(invite.inviteId, 128) ||
    !Array.isArray(invite.addresses) ||
    invite.addresses.length > 32 ||
    !Number.isInteger(invite.port) ||
    invite.port < 1 ||
    invite.port > 65535 ||
    typeof invite.secret !== "string" ||
    invite.secret.length < 40 ||
    typeof invite.expiresAt !== "number"
  ) {
    throw new Error("That Perpetual invite is malformed or incompatible.");
  }
  return invite;
}

function lanAddresses(): string[] {
  const addresses: string[] = [];
  for (const entries of Object.values(os.networkInterfaces())) {
    for (const entry of entries ?? []) {
      if (!entry.internal && entry.family === "IPv4") addresses.push(entry.address);
    }
  }
  return uniqueStrings(addresses);
}

function deriveSessionKeys(
  secret: string,
  clientNonce: Buffer,
  serverNonce: Buffer,
  deviceId: string,
): SessionKeys {
  const salt = Buffer.concat([clientNonce, serverNonce]);
  const info = Buffer.from(`${PROTOCOL_LABEL}:${deviceId}`, "utf8");
  const material = Buffer.from(
    hkdfSync("sha256", Buffer.from(secret, "base64url"), salt, info, 64),
  );
  return {
    clientToServer: material.subarray(0, 32),
    serverToClient: material.subarray(32, 64),
  };
}

function encryptFrame(
  key: Buffer,
  counter: bigint,
  direction: "c2s" | "s2c",
  plaintext: string,
): EncryptedFrame {
  const nonce = counterNonce(counter);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(`${PROTOCOL_LABEL}:${direction}`, "utf8"));
  const ciphertext = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
  return {
    n: counter.toString(),
    c: Buffer.concat([ciphertext, cipher.getAuthTag()]).toString("base64url"),
  };
}

function decryptFrame(
  key: Buffer,
  expectedCounter: bigint,
  direction: "c2s" | "s2c",
  frame: EncryptedFrame,
): string {
  if (frame.n !== expectedCounter.toString()) throw new Error("out-of-order encrypted frame");
  const payload = Buffer.from(frame.c, "base64url");
  if (payload.length < 16 || payload.length > MAX_WIRE_LINE_BYTES) {
    throw new Error("invalid encrypted frame size");
  }
  const ciphertext = payload.subarray(0, -16);
  const tag = payload.subarray(-16);
  const decipher = createDecipheriv("aes-256-gcm", key, counterNonce(expectedCounter));
  decipher.setAAD(Buffer.from(`${PROTOCOL_LABEL}:${direction}`, "utf8"));
  decipher.setAuthTag(tag);
  return Buffer.concat([decipher.update(ciphertext), decipher.final()]).toString("utf8");
}

function counterNonce(counter: bigint): Buffer {
  if (counter <= 0n) throw new Error("invalid frame counter");
  const nonce = Buffer.alloc(12);
  nonce.writeBigUInt64BE(counter, 4);
  return nonce;
}

function mac(secret: string, payload: string): string {
  return createHmac("sha256", Buffer.from(secret, "base64url"))
    .update(payload, "utf8")
    .digest("base64url");
}

function verifyMac(secret: string, payload: string, value: string): void {
  const expected = Buffer.from(mac(secret, payload), "base64url");
  const actual = Buffer.from(value, "base64url");
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    throw new Error("collaboration authentication failed");
  }
}

function clientMacPayload(inviteId: string, deviceId: string, nonce: string): string {
  return `${PROTOCOL_LABEL}:client:${inviteId}:${deviceId}:${nonce}`;
}

function serverMacPayload(
  inviteId: string,
  deviceId: string,
  clientNonce: string,
  serverNonce: string,
): string {
  return `${PROTOCOL_LABEL}:server:${inviteId}:${deviceId}:${clientNonce}:${serverNonce}`;
}

function decodeFixed(value: string, bytes: number): Buffer {
  const decoded = Buffer.from(value, "base64url");
  if (decoded.length !== bytes) throw new Error("invalid nonce");
  return decoded;
}

function safeIdentifier(value: unknown, max: number): value is string {
  return (
    typeof value === "string" &&
    value.length >= 8 &&
    value.length <= max &&
    /^[A-Za-z0-9._:-]+$/.test(value)
  );
}

function uniqueStrings(values: string[]): string[] {
  return [...new Set(values.map((value) => value.trim()).filter(Boolean))];
}

async function readJsonLine<T>(socket: net.Socket, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let buffer = "";
    const timer = setTimeout(() => finish(new Error("handshake timed out")), timeoutMs);
    const onData = (chunk: Buffer | string) => {
      buffer += chunk.toString();
      if (buffer.length > 64 * 1024) return finish(new Error("handshake frame is too large"));
      const idx = buffer.indexOf("\n");
      if (idx < 0) return;
      const line = buffer.slice(0, idx).trim();
      const rest = buffer.slice(idx + 1);
      try {
        const parsed = JSON.parse(line) as T;
        // Preserve a coalesced next frame. Keep the socket paused until the
        // next reader (or encrypted stream) has installed its data listener.
        socket.pause();
        cleanup();
        if (rest) socket.unshift(Buffer.from(rest, "utf8"));
        resolve(parsed);
      } catch {
        finish(new Error("invalid handshake frame"));
      }
    };
    const onError = (error: Error) => finish(error);
    const onClose = () => finish(new Error("connection closed during handshake"));
    const cleanup = () => {
      clearTimeout(timer);
      socket.off("data", onData);
      socket.off("error", onError);
      socket.off("close", onClose);
    };
    const finish = (error: Error) => {
      cleanup();
      reject(error);
    };
    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("close", onClose);
    socket.resume();
  });
}
