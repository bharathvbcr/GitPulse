#!/usr/bin/env node
/**
 * Runs `harness/terminal-glass.html` in headless Chrome or in the system
 * WKWebView and prints, per arm, how many runs left the WebGL terminal
 * different from a single-frame render of the same bytes.
 *
 *   node scripts/glass-atlas-repro.mjs <chrome|webkit> "<query>" [deadlineSeconds]
 *
 * The page is deliberately not in BROWSER_HARNESSES: it reproduces only at
 * devicePixelRatio 2 (WKWebView on a Retina display; Chrome needs
 * `GLASS_CHROME_ARGS=--force-device-scale-factor=2`), and a runner at DPR 1
 * would pass it for the wrong reason. The page reads these query parameters:
 *
 *   arms=opaque,glass-a0,glass-a1,glass-a25,opaque-notransp   (default opaque,glass-a0)
 *   runs=5 frames=150 pace=1 ops=3   stream shape; seeds 1..runs
 *   distinct=0                       cap on distinct colours (0 = unbounded)
 *   clearEvery=0                     clearTextureAtlas() every N chunks while streaming
 *   scroll=1                         read the history back and forth after the stream
 *   debug=1                          write PNG strips of the first differing rows to tmp/
 *
 * `GLASS_XTERM=<dir holding @xterm/xterm and @xterm/addon-webgl>` runs the page
 * against that build without touching package.json.
 */
import { execFile, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromeBinary, chromeLaunch } from "./browser-regressions.mjs";

const PAGE = "/harness/terminal-glass.html";

/**
 * @typedef {{ arm: string, seed: number, dpr: number, sig: string, blankPixels: number,
 *   twinPixels: number, twinRows: number, twinRepeatPixels: number, afterRefreshPixels: number,
 *   afterAtlasPixels: number, webgl: boolean, strips?: string[] }} Row
 */

/** One line per arm: how many runs are corrupt, by how much, and what heals them.
 * @param {Row[]} rows */
export function summarize(rows) {
  /** @type {Map<string, Row[]>} */
  const arms = new Map();
  for (const row of rows) arms.set(row.arm, [...(arms.get(row.arm) ?? []), row]);
  return [...arms].map(([arm, runs]) => {
    const bad = runs.filter((row) => row.twinPixels > 0);
    const px = runs.map((row) => row.twinPixels).sort((a, b) => a - b);
    return [
      arm.padEnd(16), `n=${runs.length}`, `corrupt=${bad.length}/${runs.length}`,
      `px[min/med/max]=${px[0]}/${px[Math.floor(px.length / 2)]}/${px.at(-1)}`,
      `blankPx=${runs.reduce((sum, row) => sum + row.blankPixels, 0)}`,
      `yardstickWobble=${runs.reduce((sum, row) => sum + row.twinRepeatPixels, 0)}`,
      `refreshHeals=${bad.filter((row) => row.afterRefreshPixels === 0).length}/${bad.length}`,
      `atlasHeals=${bad.filter((row) => row.afterAtlasPixels === 0).length}/${bad.length}`,
      `dpr=${runs[0].dpr}`, `build=${runs[0].sig}`,
    ].join(" ");
  });
}

async function main() {
  const [engine = "chrome", query = "", seconds = "400"] = process.argv.slice(2);
  if (engine !== "chrome" && engine !== "webkit") throw new Error("engine is chrome or webkit");
  if (engine === "webkit" && process.platform !== "darwin") throw new Error("The WebKit runner requires macOS");
  const deadline = Number(seconds);
  const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-glass-"));
  const reportPath = `/__gp_result/${randomUUID()}`;
  /** @type {import('node:child_process').ChildProcess | undefined} */
  let browser;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let timer;
  /** @type {(html: string) => void} */
  let deliver = () => {};
  const verdict = new Promise((resolve, reject) => {
    deliver = resolve;
    timer = setTimeout(() => reject(new Error(`no verdict in ${deadline}s`)), deadline * 1000);
  });
  verdict.catch(() => {});

  /** @type {import('vite').Plugin} */
  const receiver = {
    name: "glass-verdict",
    configureServer(server) {
      server.middlewares.use(reportPath, (request, response) => {
        let html = "";
        request.setEncoding("utf8");
        request.on("data", (chunk) => { html += chunk; });
        request.on("end", () => { response.end("ok"); deliver(html); });
      });
    },
  };
  const xterm = process.env.GLASS_XTERM ? path.resolve(process.env.GLASS_XTERM, "@xterm") : null;
  if (xterm) {
    for (const name of ["xterm", "addon-webgl"]) {
      console.log(name, JSON.parse(readFileSync(path.join(xterm, name, "package.json"), "utf8")).version);
    }
  }
  const server = await createServer({
    resolve: xterm ? { alias: [{ find: /^@xterm\/(xterm|addon-webgl)(.*)$/, replacement: `${xterm}/$1$2` }] } : {},
    plugins: [receiver],
    cacheDir: path.join(profile, "vite-cache"),
    optimizeDeps: { entries: ["harness/terminal-glass.html"] },
    server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
  });
  try {
    await server.listen();
    const address = server.httpServer?.address();
    if (!address || typeof address === "string") throw new Error("Vite did not bind a test port");
    const url = `http://127.0.0.1:${address.port}${PAGE}?check=1&report=${reportPath}&${query}`;
    if (engine === "webkit") {
      // The shipped Swift runner only opens pages in BROWSER_HARNESSES; this one
      // is not, so run a copy whose allow-list also names it.
      const source = fileURLToPath(new URL("./webkit-regressions.swift", import.meta.url));
      const patched = readFileSync(source, "utf8").replace(/"\/harness\/mutation-routing\.html"\]/, `"/harness/mutation-routing.html", "${PAGE}"]`);
      if (!patched.includes(PAGE)) throw new Error("webkit-regressions.swift no longer has the allow-list this script patches");
      const copy = path.join(profile, "webkit-glass.swift");
      writeFileSync(copy, patched);
      browser = execFile("xcrun", ["swift", copy, url, String(deadline + 5)], { timeout: (deadline + 5) * 1000, maxBuffer: 1 << 20, killSignal: "SIGKILL" });
    } else {
      const launch = chromeLaunch(chromeBinary(), profile, url);
      const extra = (process.env.GLASS_CHROME_ARGS ?? "").split(" ").filter(Boolean);
      browser = spawn(launch.file, [...launch.args.slice(0, -1), ...extra, url], { stdio: "ignore", detached: true });
    }
    const html = await verdict;
    const encoded = /data-gp-result="([^"]*)"/.exec(html)?.[1];
    if (!encoded) throw new Error("the page did not report");
    const result = JSON.parse(decodeURIComponent(encoded));
    /** @type {Row[]} */
    const rows = JSON.parse(result.diagnostics);
    const failed = result.results.filter((/** @type {{ pass: boolean }} */ check) => !check.pass);
    console.log(JSON.stringify({ engine, query, checks: result.results.length, failed, errors: result.errors }));
    mkdirSync("tmp", { recursive: true });
    for (const { strips, ...row } of rows) {
      console.log(JSON.stringify(row));
      (strips ?? []).forEach((strip, i) => {
        const file = `tmp/strip-${engine}-${row.arm}-${row.seed}-${i}.png`;
        writeFileSync(file, Buffer.from(strip.split(",")[1], "base64"));
        console.log("wrote", file);
      });
    }
    for (const line of summarize(rows)) console.log(line);
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

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
