// Slow-switching WebGL context bench (WebKit). Not part of CI or `test:browser`.
//
//   node scripts/churn-bench.mjs --tier compact|roomy --output idle|stream [--phases 20:600,30:600] [--out file.json]
//
// Serves `harness/churn-bench.html` from a throwaway Vite server, loads it in a
// WKWebView (`scripts/churn-bench.swift`), prints each switch as it happens and
// a table at the end. `--phases interval_s:duration_s,...` defaults to a switch
// every 20 s for 10 minutes, then every 30 s for 10 minutes. A short smoke run:
// `--phases 2:20`. Check the machine's load before starting; the page measures
// what the engine does at human pace, and a loaded host changes that.
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { rmSync, writeFileSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

/** @param {string} name @param {string} fallback */
function arg(name, fallback) {
  const at = process.argv.indexOf(`--${name}`);
  return at === -1 ? fallback : process.argv[at + 1];
}

/** The tables the report prints, from the page's summary. @param {any} summary */
export function renderTables(summary) {
  const rows = summary.phases.map((/** @type {{ every_s: number, minutes: number, switches: number, created: number, forced: string[] }} */ phase) => `| every ${phase.every_s} s for ${phase.minutes} min | ${phase.switches} | ${phase.created} | ${phase.forced.length} | ${phase.forced.length ? phase.forced.join(", ") : "none"} |`);
  return [
    `${summary.tier} tier, ${summary.output === "stream" ? "one pane receiving steady output" : "page otherwise idle"} (cap ${summary.pool.cap}, hold ${summary.pool.hold_ms / 1000} s, window max ${summary.pool.max})`,
    "",
    "| schedule | switches | contexts created | forced losses | pane lost |",
    "| --- | --- | --- | --- | --- |",
    ...rows,
    "",
    `idle pane final renderer: ${summary.idle_final}${summary.idle_context_lost ? " (its context is lost)" : ""}; docks: ${summary.dock_final.join("/")}; total created ${summary.total_created}, forced ${summary.total_forced}, ${summary.elapsed_s} s`,
  ].join("\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.platform !== "darwin") throw new Error("The bench needs the macOS WebKit launcher");
  const tier = arg("tier", "compact");
  const output = arg("output", "idle");
  const phases = arg("phases", "20:600,30:600");
  const seconds = phases.split(",").reduce((/** @type {number} */ sum, /** @type {string} */ phase) => sum + Number(phase.split(":")[1]), 0);
  const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-churn-bench-"));
  const reportPath = `/__gp_bench/${randomUUID()}`;
  /** @type {(summary: unknown) => void} */
  let finish = () => {};
  const done = new Promise((resolve) => { finish = resolve; });
  const receiver = {
    name: "gitpulse-bench-receiver",
    /** @param {import("vite").ViteDevServer} server */
    configureServer(server) {
      server.middlewares.use(reportPath, (request, response) => {
        let body = "";
        request.setEncoding("utf8");
        request.on("data", (/** @type {string} */ chunk) => { body += chunk; });
        request.on("end", () => {
          response.end("ok");
          if (request.url?.startsWith("/progress")) console.log(body);
          else finish(JSON.parse(body));
        });
      });
    },
  };
  const server = await createServer({
    plugins: [receiver],
    cacheDir: path.join(profile, "vite-cache"),
    optimizeDeps: { entries: ["harness/churn-bench.html"] },
    server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
  });
  /** @type {import("node:child_process").ChildProcess | undefined} */
  let browser;
  try {
    await server.listen();
    const address = server.httpServer?.address();
    if (!address || typeof address === "string") throw new Error("Vite did not bind a port");
    const url = `http://127.0.0.1:${address.port}/harness/churn-bench.html?tier=${tier}&output=${output}&phases=${phases}&report=${reportPath}`;
    browser = execFile("xcrun", ["swift", fileURLToPath(new URL("./churn-bench.swift", import.meta.url)), url, String(seconds + 300)],
      { maxBuffer: 1024 * 1024, killSignal: "SIGKILL" });
    const exited = new Promise((_, reject) => browser?.once("exit", (code) => reject(new Error(`WebKit exited before the summary (${code})`))));
    const summary = await Promise.race([done, exited]);
    const out = arg("out", "");
    if (out) writeFileSync(out, JSON.stringify(summary, null, 2));
    console.log(renderTables(summary));
  } finally {
    if (browser && browser.exitCode === null && browser.signalCode === null) browser.kill("SIGKILL");
    await server.close();
    rmSync(profile, { recursive: true, force: true, maxRetries: 3 });
  }
  process.exit(0);
}
