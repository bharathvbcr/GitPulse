#!/usr/bin/env node
// Drive workbench-preview.mjs enhancement checks headlessly and print the verdict.
import { execFile, spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const dcstore = process.argv[2] || process.env.GITPULSE_WORKBENCH_TEST_DCSTORE || `${process.env.HOME}/.local/bin/dcstore`;
const manvi = process.argv[3] || process.env.GITPULSE_WORKBENCH_TEST_MANVI || process.env.GITPULSE_MANVI_BIN || `${process.env.HOME}/.local/bin/manvi`;
const previewScript = fileURLToPath(new URL("./workbench-preview.mjs", import.meta.url));
const chrome = process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const profile = await mkdtemp(join(tmpdir(), "gp-wb-chrome-"));

/** @type {import('node:child_process').ChildProcess | null} */
let preview = null;
/** @type {import('node:child_process').ChildProcess | null} */
let browser = null;
/** @type {import('node:http').Server | null} */
let reportServer = null;

async function cleanup() {
  if (browser && browser.exitCode === null) {
    const closed = new Promise((resolve) => browser?.once("close", resolve));
    browser.kill("SIGKILL");
    await closed;
  }
  if (preview && preview.exitCode === null) {
    const closed = new Promise((resolve) => preview?.once("close", resolve));
    preview.kill("SIGTERM");
    await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 3000))]);
    if (preview.exitCode === null) preview.kill("SIGKILL");
  }
  await new Promise((resolve) => reportServer?.close(() => resolve(undefined)));
  await rm(profile, { recursive: true, force: true, maxRetries: 3 });
}

process.on("SIGINT", () => { void cleanup().then(() => process.exit(130)); });
process.on("SIGTERM", () => { void cleanup().then(() => process.exit(143)); });

try {
  const reportPath = "/__gp_workbench_result";
  /** @type {(value: string) => void} */
  let resolveVerdict;
  /** @type {(reason: Error) => void} */
  let rejectVerdict;
  const verdict = new Promise((resolve, reject) => {
    resolveVerdict = resolve;
    rejectVerdict = reject;
    setTimeout(() => reject(new Error("Workbench enhancement checks did not finish within 120 seconds")), 120_000);
  });
  reportServer = createServer((req, res) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "POST, OPTIONS");
    res.setHeader("Access-Control-Allow-Headers", "Content-Type");
    if (req.method === "OPTIONS") { res.writeHead(204).end(); return; }
    if (req.method !== "POST" || req.url !== reportPath) { res.writeHead(404).end(); return; }
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
      if (body.length > 8 * 1024 * 1024) { req.destroy(); rejectVerdict(new Error("Verdict exceeded 8 MiB")); }
    });
    req.on("end", () => {
      res.end("received");
      const encoded = /data-gp-result="([^"]*)"/.exec(body)?.[1];
      if (!encoded) { rejectVerdict(new Error("Missing data-gp-result in report")); return; }
      try {
        const parsed = JSON.parse(decodeURIComponent(encoded));
        const rows = Array.isArray(parsed?.results) ? parsed.results : [];
        if (!rows.length || rows.some((/** @type {{ pass?: boolean }} */ row) => !row?.pass)) {
          rejectVerdict(new Error(`Enhancement checks failed: ${JSON.stringify(parsed)}`));
          return;
        }
        resolveVerdict(`${rows.length}/${rows.length} enhancement checks passed\n${rows.map((/** @type {{ name: string }} */ row) => row.name).join("\n")}`);
      } catch (error) {
        rejectVerdict(error instanceof Error ? error : new Error(String(error)));
      }
    });
  });
  await new Promise((resolve, reject) => {
    reportServer?.once("error", reject);
    reportServer?.listen(0, "127.0.0.1", () => resolve(undefined));
  });
  const reportAddress = reportServer.address();
  if (!reportAddress || typeof reportAddress === "string") throw new Error("Report server has no address");
  const reportUrl = `http://127.0.0.1:${reportAddress.port}${reportPath}`;

  let previewUrl = "";
  preview = spawn(process.execPath, [previewScript, dcstore, manvi], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    stdio: ["ignore", "pipe", "pipe"],
  });
  const previewReady = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("workbench-preview did not print a Preview URL")), 60_000);
    let buf = "";
    /** @param {Buffer | string} chunk */
    const onData = (chunk) => {
      buf += chunk.toString();
      process.stdout.write(chunk);
      const match = /Preview: (http:\/\/127\.0\.0\.1:\d+\/harness\/workbench\.html)/.exec(buf);
      if (match) { clearTimeout(timer); previewUrl = match[1]; resolve(undefined); }
    };
    preview?.stdout?.on("data", onData);
    preview?.stderr?.on("data", (chunk) => process.stderr.write(chunk));
    preview?.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`workbench-preview exited early (${code})`));
    });
  });
  await previewReady;

  await run(chrome, ["--version"], { timeout: 10_000 });
  const url = `${previewUrl}?check=1&report=${encodeURIComponent(reportUrl)}`;
  browser = execFile(chrome, [
    "--headless=new", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--window-size=1400,1000", url,
  ], { timeout: 125_000, maxBuffer: 1024 * 1024, killSignal: "SIGKILL" });
  browser.unref?.();
  console.log(await verdict);
  process.exitCode = 0;
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
} finally {
  await cleanup();
}
