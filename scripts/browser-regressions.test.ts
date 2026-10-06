import { describe, expect, it } from "vitest";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { BROWSER_HARNESSES, chromeBinary, readBrowserVerdict } from "./browser-regressions.mjs";

describe("browser regression gate", () => {
  it("keeps the native WebKit entrypoint aligned with every supported harness", () => {
    const native = readFileSync(new URL("./webkit-regressions.swift", import.meta.url), "utf8");
    const paths = [...native.matchAll(/"\/harness\/([^"/]+)\.html"/g)].map(match => match[1]);
    expect(paths.sort()).toEqual([...BROWSER_HARNESSES].sort());
  });
  const html = (rows: unknown[]) => `<html data-gp-result="${encodeURIComponent(JSON.stringify({ results: rows }))}">`;
  it("requires an executed complete verdict", () => {
    expect(() => readBrowserVerdict("<html><pre>Running…</pre></html>")).toThrow();
    expect(() => readBrowserVerdict(html([]))).toThrow();
    expect(() => readBrowserVerdict(html([{ pass: true }]))).toThrow();
  });
  it("refuses a failed or malformed assertion among passing rows", () => {
    for (const row of [{ pass: false }, { pass: "true" }, null]) {
      expect(() => readBrowserVerdict(html([...Array.from({ length: 24 }, () => ({ pass: true })), row]))).toThrow();
    }
  });
  it("accepts the completed real-browser assertion set", () => {
    expect(readBrowserVerdict(html(Array.from({ length: 24 }, () => ({ pass: true }))))).toBe("24/24 browser regressions passed");
  });
});

describe("failure reporting a CI reader can see", () => {
  it("names the failing checks, crashes and unexpected commands", async () => {
    const { failedCheckNames } = await import("./browser-regressions.mjs");
    const rows = Array.from({ length: 24 }, (_, i) => ({ name: `check ${i}`, pass: i !== 3 }));
    const result = { results: rows, crashes: [{ message: "boom" }], unexpected: ["cmd_x", "cmd_x"] };
    expect(failedCheckNames(result, rows)).toEqual([
      "check 3",
      'crash: {"message":"boom"}',
      "unexpected commands: cmd_x",
    ]);
  });

  it("escapes an annotation so its text cannot end the command early", async () => {
    const { annotation } = await import("./browser-regressions.mjs");
    expect(annotation("Chrome a:b,c", "one\ntwo 50%")).toBe("::error title=Chrome a%3Ab%2Cc::one%0Atwo 50%25");
  });
});

describe("a browser cannot outlive its runner", () => {
  const chrome = chromeBinary();
  // Reported as skipped where there is no Chrome, never as passed.
  it.skipIf(!existsSync(chrome))("Chrome exits when the runner is killed before its cleanup can run", async () => {
    const profile = mkdtempSync(path.join(tmpdir(), "gitpulse-browser-orphan-"));
    const runner = fileURLToPath(new URL("./browser-regressions.mjs", import.meta.url));
    // A stand-in runner: launches Chrome exactly as a harness run does, then
    // is SIGKILLed, so no `finally`, handler or `exit` hook can run.
    const parent = spawn(process.execPath, ["--input-type=module", "-e", `
      import { spawn } from "node:child_process";
      const { chromeLaunch } = await import(${JSON.stringify(pathToFileURL(runner).href)});
      const launch = chromeLaunch(${JSON.stringify(chrome)}, ${JSON.stringify(profile)}, "about:blank");
      const child = spawn(launch.file, launch.args, { stdio: launch.stdio });
      console.log(String(child.pid));
      setInterval(() => {}, 1000);
    `], { stdio: ["ignore", "pipe", "inherit"] });
    let pid = 0;
    const alive = () => { try { process.kill(pid, 0); return true; } catch { return false; } };
    try {
      pid = await new Promise<number>((resolve, reject) => {
        parent.stdout.once("data", (d: Buffer) => resolve(Number(String(d).trim())));
        parent.once("exit", code => reject(new Error(`stand-in runner exited (${code})`)));
      });
      expect(pid).toBeGreaterThan(0);
      // Chrome is up and waiting on its control pipe before the runner dies.
      await new Promise(r => setTimeout(r, 1500));
      expect(alive()).toBe(true);
      parent.kill("SIGKILL");
      const deadline = Date.now() + 10_000;
      while (alive() && Date.now() < deadline) await new Promise(r => setTimeout(r, 100));
      expect(alive(), `Chrome ${pid} outlived its killed runner`).toBe(false);
    } finally {
      if (pid && alive()) process.kill(pid, "SIGKILL");
      parent.kill("SIGKILL");
      rmSync(profile, { recursive: true, force: true, maxRetries: 3 });
    }
  }, 30_000);
});
