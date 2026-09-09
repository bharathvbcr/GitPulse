import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { createServer } from "vite";

const run = promisify(execFile);
export const BROWSER_HARNESSES = Object.freeze(["diagnostics", "conflicts", "uncommitted", "coverage", "branches", "hygiene", "palette"]);

/** A missing/partial verdict is a failure, even when Chrome exits normally.
 * @param {string} html
 */
export function readBrowserVerdict(html) {
  const encoded = /<html\b[^>]*\bdata-gp-result="([^"]*)"/.exec(html)?.[1];
  if (!encoded) throw new Error("Browser did not finish the diagnostics harness");
  /** @type {unknown} */
  const result = JSON.parse(decodeURIComponent(encoded));
  if (!result || typeof result !== "object" || !("results" in result) || !Array.isArray(result.results)) {
    throw new Error("Invalid browser verdict");
  }
  /** @type {unknown[]} */
  const rows = result.results;
  if (rows.length < 24 || rows.some(row => !row || typeof row !== "object" || !("pass" in row) || row.pass !== true)) {
    throw new Error(`Browser regressions failed: ${JSON.stringify(result)}`);
  }
  return `${rows.length}/${rows.length} browser regressions passed`;
}

async function main() {
  const webkit = process.argv.includes("--webkit");
  const harnessIndex = process.argv.indexOf("--harness");
  const harness = harnessIndex === -1 ? "diagnostics" : process.argv[harnessIndex + 1];
  if (!BROWSER_HARNESSES.includes(harness)) throw new Error(`Unknown browser harness; use ${BROWSER_HARNESSES.join(", ")}`);
  if (webkit && process.platform !== "darwin") throw new Error("The WebKit regression runner requires macOS");
  const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-browser-"));
  /** @type {import('vite').ViteDevServer | undefined} */
  let server;
  const reportPath = `/__gp_result/${randomUUID()}`;
  /** @type {import('node:child_process').ChildProcess | undefined} */
  let browser;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let deadline;
  let requests = 0;
  let lastRequest = "none";
  /** @type {import('vite').Plugin} */
  const receiver = { name: "gitpulse-browser-verdict" };
  const completed = new Promise((resolve, reject) => {
    deadline = setTimeout(() => reject(new Error(`Browser did not finish within 60 seconds (${requests} HTTP requests; last: ${lastRequest})`)), 60_000);
    receiver.configureServer = (server) => {
      server.middlewares.use((request, _response, next) => { requests++; lastRequest = request.url ?? "unknown"; next(); });
      server.middlewares.use(reportPath, (request, response) => {
      if (request.method !== "POST") { response.statusCode = 405; response.end(); return; }
      let html = "";
      request.setEncoding("utf8");
      request.on("data", chunk => {
        html += chunk;
        if (html.length > 8 * 1024 * 1024) { request.destroy(); reject(new Error("Browser verdict exceeded 8 MiB")); }
      });
      request.on("error", reject);
      request.on("end", () => {
        response.end("received");
        try { resolve(readBrowserVerdict(html)); } catch (error) { reject(error); }
      });
      });
    };
  });
  // Attach a handler before server startup can fail or exceed the deadline.
  void completed.catch(() => {});
  try {
    server = await createServer({
      plugins: [receiver],
      cacheDir: path.join(profile, "vite-cache"),
      // Production has explicit entry points, and fixtures select components
      // dynamically. Discover their dependencies before any fixture starts.
      optimizeDeps: {
        entries: [`harness/${harness}.html`, "src/lib/components/**/*.svelte"],
      },
      server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
    });
    await server.listen();
    const address = server.httpServer?.address();
    if (!address || typeof address === "string") throw new Error("Vite did not bind a test port");
    const chrome = process.env.CHROME_BIN || (process.platform === "darwin"
      ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
      : process.platform === "win32" ? "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe" : "google-chrome");
    const version = await run(webkit ? "xcrun" : chrome, webkit ? ["swift", "--version"] : ["--version"], { timeout: 10_000 });
    console.log(version.stdout.trim());
    const url = `http://127.0.0.1:${address.port}/harness/${harness}.html?check=1&report=${reportPath}`;
    browser = execFile(webkit ? "xcrun" : chrome, webkit ? ["swift", fileURLToPath(new URL("./webkit-regressions.swift", import.meta.url)), url] : [
      "--headless", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
      "--disable-background-networking", "--window-size=1400,1000",
      url,
    ], { timeout: 65_000, maxBuffer: 1024 * 1024, killSignal: "SIGKILL" });
    const exited = new Promise((_, reject) => {
      browser?.once("error", reject);
      browser?.once("exit", code => reject(new Error(`${webkit ? "WebKit" : "Chrome"} exited before the verdict (${code})`)));
    });
    console.log(await Promise.race([completed, exited]));
  } finally {
    clearTimeout(deadline);
    if (browser && browser.exitCode === null) {
      const closed = new Promise(resolve => browser?.once("close", resolve));
      browser.kill("SIGKILL");
      await closed;
    }
    await server?.close();
    await rm(profile, { recursive: true, force: true, maxRetries: 3 });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
