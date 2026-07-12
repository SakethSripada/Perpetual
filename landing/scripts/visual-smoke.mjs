import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { chromium } from "playwright";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = resolve(__dirname, "..");
const port = Number(process.env.LANDING_PORT ?? "4173");
const url = `http://127.0.0.1:${port}/`;
const artifactDir = resolve(root, "artifacts", "visual-smoke");

const server = spawn(
  process.platform === "win32" ? "npm.cmd" : "npm",
  ["run", "preview", "--", "--port", String(port), "--strictPort"],
  { cwd: root, stdio: ["ignore", "pipe", "pipe"] }
);

let serverLog = "";
server.stdout.on("data", (chunk) => {
  serverLog += chunk.toString();
});
server.stderr.on("data", (chunk) => {
  serverLog += chunk.toString();
});

try {
  await waitForPreview();
  await mkdir(artifactDir, { recursive: true });

  const browser = await chromium.launch({ headless: true });
  const reports = [];

  for (const target of [
    { name: "desktop", width: 1440, height: 1100, reducedMotion: "no-preference" },
    { name: "laptop", width: 1280, height: 900, reducedMotion: "no-preference" },
    { name: "mobile", width: 390, height: 900, reducedMotion: "no-preference" },
    { name: "reduced-motion", width: 1280, height: 900, reducedMotion: "reduce" },
  ]) {
    const page = await browser.newPage({
      viewport: { width: target.width, height: target.height },
      deviceScaleFactor: 1,
    });
    await page.emulateMedia({ reducedMotion: target.reducedMotion });

    const messages = [];
    page.on("console", (message) => {
      if (["error", "warning"].includes(message.type())) {
        messages.push(`${message.type()}: ${message.text()}`);
      }
    });
    page.on("pageerror", (error) => {
      messages.push(`pageerror: ${error.message}`);
    });

    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(300);
    await page.screenshot({
      path: resolve(artifactDir, `${target.name}.png`),
      fullPage: false,
    });

    const report = await page.evaluate(() => {
      const doc = document.documentElement;
      const hero = document.querySelector(".hero")?.getBoundingClientRect();
      const logoBand = document.querySelector(".logo-band")?.getBoundingClientRect();
      const handoff = document.querySelector(".handoff-card")?.getBoundingClientRect();
      const h1 = document.querySelector("h1")?.getBoundingClientRect();
      const agentPills = Array.from(document.querySelectorAll(".agent-pill")).filter((node) => {
        const rect = node.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      }).length;
      const logos = Array.from(document.querySelectorAll(".logo-strip img")).filter((node) => {
        const rect = node.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      }).length;
      const extensionPanel = document.querySelector(".extension-panel")?.getBoundingClientRect();

      const buttons = Array.from(document.querySelectorAll(".button")).map((button) => {
        const rect = button.getBoundingClientRect();
        const style = getComputedStyle(button);
        return {
          text: button.textContent?.trim() ?? "",
          width: rect.width,
          height: rect.height,
          display: style.display,
        };
      });

      return {
        overflowX: doc.scrollWidth - doc.clientWidth,
        heroHeight: hero?.height ?? null,
        logoBandTop: logoBand?.top ?? null,
        handoff: handoff ? { width: handoff.width, height: handoff.height, top: handoff.top } : null,
        h1: h1 ? { left: h1.left, right: h1.right, width: h1.width } : null,
        agentPills,
        logos,
        hasExtensionPanel: Boolean(extensionPanel && extensionPanel.width > 200),
        buttons,
        bodyTextLength: document.body.innerText.length,
      };
    });

    assert(messages.length === 0, `${target.name}: console/page errors:\n${messages.join("\n")}`);
    assert(report.overflowX === 0, `${target.name}: horizontal overflow ${report.overflowX}px`);
    assert(report.heroHeight && report.heroHeight > 420, `${target.name}: hero did not render`);
    assert(
      report.logoBandTop !== null && report.logoBandTop > 300,
      `${target.name}: logo band overlaps the hero`
    );
    assert(report.handoff && report.handoff.width > 250 && report.handoff.height > 180, `${target.name}: handoff card missing`);
    assert(report.h1 && report.h1.left >= 0 && report.h1.right <= target.width, `${target.name}: hero title overflows viewport`);
    if (target.width > 760) {
      assert(report.agentPills >= 3, `${target.name}: agent handoff pills missing`);
    }
    assert(report.logos >= 6, `${target.name}: tool logos missing`);
    assert(report.hasExtensionPanel, `${target.name}: extension panel missing`);
    assert(report.bodyTextLength > 1500, `${target.name}: page text did not render`);
    for (const button of report.buttons) {
      if (button.display === "none") continue;
      assert(button.width >= 140 && button.height >= 44, `${target.name}: CTA too small: ${button.text}`);
    }

    reports.push({ target, ...report });
    await page.close();
  }

  await browser.close();
  console.log(JSON.stringify(reports, null, 2));
} finally {
  server.kill("SIGTERM");
}

async function waitForPreview() {
  const started = Date.now();
  while (Date.now() - started < 15_000) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 250));
    }
  }

  throw new Error(`Preview server did not start at ${url}\n${serverLog}`);
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}
