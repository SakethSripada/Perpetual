import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
test("attachment-only and edited messages retain bytes while display text hides the transport", async () => {
  const dir = mkdtempSync(path.join(tmpdir(), "perpetual-attachments-"));
  try {
    const out = path.join(dir, "helper.cjs");
    await build({entryPoints:["webview/src/attachments.ts"], outfile:out, bundle:true, platform:"node", format:"cjs"});
    const {packMessage, unpackMessage, displayMessage} = require(out);
    const files = [{id:"file",name:"scan.pdf",mime:"application/pdf",data:"JVBERi0xLjc="}];
    const message = packMessage("",files).trim();
    assert.deepEqual(unpackMessage(message).attachments, files);
    assert.deepEqual(unpackMessage(packMessage("Revised",unpackMessage(message).attachments)).attachments, files);
    assert.match(displayMessage(message), /scan.pdf/);
    assert.doesNotMatch(displayMessage(message), /JVBERi0xLjc=/);
  } finally { rmSync(dir,{recursive:true,force:true}); }
});
