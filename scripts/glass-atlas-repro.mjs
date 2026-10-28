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
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runHarnessPage } from "./harness-page-runner.mjs";

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
  const xtermDir = process.env.GLASS_XTERM ?? null;
  if (xtermDir) {
    for (const name of ["xterm", "addon-webgl"]) {
      console.log(name, JSON.parse(readFileSync(path.resolve(xtermDir, "@xterm", name, "package.json"), "utf8")).version);
    }
  }
  const { results, errors, rows } = await runHarnessPage({
    engine: /** @type {"chrome" | "webkit"} */ (engine), page: PAGE, query, deadlineSeconds: Number(seconds), xtermDir,
    chromeArgs: (process.env.GLASS_CHROME_ARGS ?? "").split(" ").filter(Boolean),
  });
  const failed = results.filter((check) => !check.pass);
  console.log(JSON.stringify({ engine, query, checks: results.length, failed, errors }));
  mkdirSync("tmp", { recursive: true });
  for (const { strips, ...row } of /** @type {Row[]} */ (rows)) {
    console.log(JSON.stringify(row));
    (strips ?? []).forEach((strip, i) => {
      const file = `tmp/strip-${engine}-${row.arm}-${row.seed}-${i}.png`;
      writeFileSync(file, Buffer.from(strip.split(",")[1], "base64"));
      console.log("wrote", file);
    });
  }
  for (const line of summarize(rows)) console.log(line);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
