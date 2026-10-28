/**
 * Runs one measurement page under `harness/` in headless Chrome or in the
 * system WKWebView and returns what the page reported. The measurement pages
 * (`terminal-glass.html`, `terminal-switch.html`) are deliberately not in
 * BROWSER_HARNESSES: they report numbers for a person to read, not checks a
 * gate can pass, so `browser-regressions.mjs` never opens them.
 *
 * A page reports the way the regression pages do: `data-gp-result` on the
 * document element holds `{ results, errors, diagnostics }`, with its rows as
 * the JSON string in `diagnostics`, and the page POSTs its outer HTML to the
 * `report` path it was given.
 *
 * Vite serves from `process.cwd()`, so run the script from the checkout whose
 * code it should measure.
 */
import { execFile, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromeBinary, chromeLaunch } from "./browser-regressions.mjs";

/**
 * @param {{ engine: "chrome" | "webkit", page: string, query: string, deadlineSeconds: number,
 *   xtermDir?: string | null, chromeArgs?: string[] }} options
 *   `page` is the served path (`/harness/x.html`); `xtermDir` is a directory
 *   holding `@xterm/xterm` and `@xterm/addon-webgl` to measure instead of the
 *   installed ones.
 * @returns {Promise<{ results: { name: string, pass: boolean }[], errors: string[], rows: any[] }>}
 */
export async function runHarnessPage({ engine, page, query, deadlineSeconds, xtermDir = null, chromeArgs = [] }) {
  if (engine !== "chrome" && engine !== "webkit") throw new Error("engine is chrome or webkit");
  if (engine === "webkit" && process.platform !== "darwin") throw new Error("The WebKit runner requires macOS");
  const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-measure-"));
  const reportPath = `/__gp_result/${randomUUID()}`;
  /** @type {import('node:child_process').ChildProcess | undefined} */
  let browser;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let timer;
  /** @type {(html: string) => void} */
  let deliver = () => {};
  /** @type {Promise<string>} */
  const verdict = new Promise((resolve, reject) => {
    deliver = resolve;
    timer = setTimeout(() => reject(new Error(`no verdict in ${deadlineSeconds}s`)), deadlineSeconds * 1000);
  });
  verdict.catch(() => {});

  /** @type {import('vite').Plugin} */
  const receiver = {
    name: "measure-verdict",
    configureServer(server) {
      server.middlewares.use(reportPath, (request, response) => {
        let html = "";
        request.setEncoding("utf8");
        request.on("data", (chunk) => { html += chunk; });
        request.on("end", () => { response.end("ok"); deliver(html); });
      });
    },
  };
  const xterm = xtermDir ? path.resolve(xtermDir, "@xterm") : null;
  const server = await createServer({
    resolve: xterm ? { alias: [{ find: /^@xterm\/(xterm|addon-webgl)(.*)$/, replacement: `${xterm}/$1$2` }] } : {},
    plugins: [receiver],
    cacheDir: path.join(profile, "vite-cache"),
    optimizeDeps: { entries: [page.replace(/^\//, "")] },
    server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
  });
  try {
    await server.listen();
    const address = server.httpServer?.address();
    if (!address || typeof address === "string") throw new Error("Vite did not bind a test port");
    const url = `http://127.0.0.1:${address.port}${page}?check=1&report=${reportPath}&${query}`;
    if (engine === "webkit") {
      // The shipped Swift runner only opens pages in BROWSER_HARNESSES; this one
      // is not, so run a copy whose allow-list also names it.
      const source = fileURLToPath(new URL("./webkit-regressions.swift", import.meta.url));
      const patched = readFileSync(source, "utf8").replace(/"\/harness\/mutation-routing\.html"\]/, `"/harness/mutation-routing.html", "${page}"]`);
      if (!patched.includes(page)) throw new Error("webkit-regressions.swift no longer has the allow-list this script patches");
      const copy = path.join(profile, "webkit-measure.swift");
      writeFileSync(copy, patched);
      browser = execFile("xcrun", ["swift", copy, url, String(deadlineSeconds + 5)], { timeout: (deadlineSeconds + 5) * 1000, maxBuffer: 1 << 20, killSignal: "SIGKILL" });
    } else {
      const launch = chromeLaunch(chromeBinary(), profile, url);
      browser = spawn(launch.file, [...launch.args.slice(0, -1), ...chromeArgs, url], { stdio: "ignore", detached: true });
    }
    const html = await verdict;
    const encoded = /data-gp-result="([^"]*)"/.exec(html)?.[1];
    if (!encoded) throw new Error("the page did not report");
    const result = JSON.parse(decodeURIComponent(encoded));
    return { results: result.results, errors: result.errors, rows: JSON.parse(result.diagnostics) };
  } finally {
    clearTimeout(timer);
    if (browser && browser.exitCode === null && browser.signalCode === null) {
      const closed = new Promise((resolve) => browser?.once("close", resolve));
      // Chrome was started as its own process group; its helpers go with it.
      // The pid is the one this script spawned a moment ago, never a name match.
      if (engine === "chrome" && browser.pid) {
        try { process.kill(-browser.pid, "SIGKILL"); } catch { browser.kill("SIGKILL"); }
      } else {
        browser.kill("SIGKILL");
      }
      await closed;
    }
    await server.close();
    await rm(profile, { recursive: true, force: true, maxRetries: 3 });
  }
}
