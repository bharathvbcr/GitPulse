// Runs one harness/stress.html query in headless Chrome and prints the probe
// result it records: `node scripts/stress-probe.mjs "<query>" [seconds]`.
//
// The browser runner (scripts/browser-regressions.mjs) loads a fixed page URL
// and expects a `results[]` verdict, so it cannot select a stress.html scenario.
// This is the same server-and-browser setup, for the scenarios that report a
// measurement rather than pass/fail checks:
//
//   node scripts/stress-probe.mjs "c=FileTreePanel&scenario=treebuilds&paths=20000&dirty=20&tabs=1&cycles=8"
//   node scripts/stress-probe.mjs "c=FileTreePanel&scenario=flushcost&paths=20000&dirty=20&tabs=1&cycles=6"
//   node scripts/stress-probe.mjs "c=RepoTabBar&scenario=flushcost&tabs=500&cycles=6"
//
// Prints one JSON line with `treebuilds` and `flushcost` (null for the scenario
// not run), and the 1-minute load average before and after: a timing taken
// above 30 is contaminated, a count is not affected by load.
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { loadavg, tmpdir } from "node:os";
import path from "node:path";
import { createServer } from "vite";
import { chromeBinary, startChrome } from "./browser-regressions.mjs";

const query = process.argv[2];
if (!query) {
  console.error('usage: node scripts/stress-probe.mjs "<stress.html query>" [seconds]');
  process.exit(2);
}
const seconds = Number(process.argv[3] ?? 240);
const loadBefore = loadavg()[0];
const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-stress-probe-"));
const reportPath = `/__gp_result/${randomUUID()}`;
/** @type {(html: string) => void} */
let resolveHtml = () => {};
/** @type {(error: Error) => void} */
let rejectHtml = () => {};
const posted = new Promise((resolve, reject) => { resolveHtml = resolve; rejectHtml = reject; });
/** @type {import("vite").Plugin} */
const receiver = {
  name: "stress-probe-verdict",
  configureServer(server) {
    server.middlewares.use(reportPath, (request, response) => {
      let html = "";
      request.setEncoding("utf8");
      request.on("data", (chunk) => { html += chunk; });
      request.on("end", () => { response.end("received"); resolveHtml(html); });
    });
  },
};
const server = await createServer({
  plugins: [receiver],
  cacheDir: path.join(profile, "vite-cache"),
  optimizeDeps: { entries: ["harness/stress.html", "src/lib/components/**/*.svelte"] },
  server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
});
await server.listen();
const address = server.httpServer?.address();
if (!address || typeof address === "string") throw new Error("Vite did not bind a test port");
const url = `http://127.0.0.1:${address.port}/harness/stress.html?${query}&report=${reportPath}`;
const browser = startChrome(chromeBinary(), profile, url);
const timer = setTimeout(() => rejectHtml(new Error(`no verdict within ${seconds}s`)), seconds * 1000);
let code = 0;
try {
  const html = await posted;
  const encoded = /data-gp-result="([^"]*)"/.exec(html)?.[1];
  if (!encoded) throw new Error("the page posted no data-gp-result");
  const result = JSON.parse(decodeURIComponent(encoded));
  const loadAfter = loadavg()[0];
  console.log(JSON.stringify({
    armed: result.armed, depthExceeded: result.depthExceeded, mountError: result.mountError, otherCrashes: result.otherCrashes,
    treebuilds: result.treebuilds, flushcost: result.flushcost,
    loadBefore, loadAfter, timingContaminated: loadBefore > 30 || loadAfter > 30,
  }));
} catch (error) {
  console.error(String(error));
  code = 1;
} finally {
  clearTimeout(timer);
  if (browser.exitCode === null && browser.signalCode === null) browser.kill("SIGKILL");
  await server.close();
  await rm(profile, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(code);
