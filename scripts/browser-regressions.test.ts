import { describe, expect, it } from "vitest";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { BROWSER_HARNESSES, DEFAULT_DEADLINE_SECONDS, HARNESS_DEADLINE_SECONDS, chromeBinary, engineDisplayName, harnessDeadlineSeconds, nativeRunnerBinary, readBrowserVerdict, resolveEngine, waitForProcessClose } from "./browser-regressions.mjs";
import { wallClockBudget } from "../src/lib/testing/wallClock";

describe("browser regression gate", () => {
  it("keeps the native WebKit entrypoint aligned with every supported harness", () => {
    const native = readFileSync(new URL("./webkit-regressions.swift", import.meta.url), "utf8");
    const paths = [...native.matchAll(/"\/harness\/([^"/]+)\.html"/g)].map(match => match[1]);
    expect(paths.sort()).toEqual([...BROWSER_HARNESSES].sort());
  });
  it("keeps the cross-platform Rust tao/wry runner aligned with every supported harness", () => {
    const runner = readFileSync(new URL("../src-tauri/examples/webview_regressions.rs", import.meta.url), "utf8");
    const paths = [...runner.matchAll(/"\/harness\/([^"/]+)\.html"/g)].map(match => match[1]);
    expect(paths.sort()).toEqual([...BROWSER_HARNESSES].sort());
    expect(runner).toContain("http://127.0.0.1:");
    expect(runner).toContain("1.0..=600.0");
    expect(runner).toContain("msSmartScreenProtection");
    expect(runner).toContain("create_dir_all");
  });
  it("selects the native shipping webview engine per OS and rejects conflicting selectors", () => {
    expect(resolveEngine([], "linux")).toBe("chrome");
    expect(resolveEngine(["--native"], "darwin")).toBe("webkit");
    expect(resolveEngine(["--native"], "linux")).toBe("webview");
    expect(resolveEngine(["--native"], "win32")).toBe("webview");
    expect(resolveEngine(["--webview"], "darwin")).toBe("webview");
    expect(resolveEngine(["--engine", "native"], "linux")).toBe("webview");
    expect(resolveEngine(["--engine", "chrome"], "win32")).toBe("chrome");
    expect(() => resolveEngine(["--webkit"], "linux")).toThrow(/requires macOS/);
    expect(() => resolveEngine(["--webkit", "--native"], "darwin")).toThrow(/at most one/);
    expect(() => resolveEngine(["--engine", "gecko"], "linux")).toThrow(/Unknown browser engine/);

    expect(engineDisplayName("webview", "linux")).toBe("WebKitGTK");
    expect(engineDisplayName("webview", "win32")).toBe("WebView2");
    expect(engineDisplayName("webkit", "darwin")).toBe("WebKit");
    expect(engineDisplayName("chrome", "linux")).toBe("Chrome");

    expect(nativeRunnerBinary("linux", {}, "/repo")).toBe(path.join("/repo", "src-tauri", "target", "debug", "examples", "webview_regressions"));
    expect(nativeRunnerBinary("win32", {}, "/repo")).toBe(path.join("/repo", "src-tauri", "target", "debug", "examples", "webview_regressions.exe"));
    expect(nativeRunnerBinary("linux", { CARGO_TARGET_DIR: "/custom-target" }, "/repo")).toBe(path.join("/custom-target", "debug", "examples", "webview_regressions"));
    expect(nativeRunnerBinary("linux", { GITPULSE_WEBVIEW_RUNNER_BIN: "/opt/bin/runner" }, "/repo")).toBe(path.resolve("/opt/bin/runner"));
  });
  it("gives a long page its own deadline and keeps the minute for every other page", () => {
    expect(harnessDeadlineSeconds("tasks")).toBe(360);
    expect(harnessDeadlineSeconds("health")).toBe(120);
    for (const harness of BROWSER_HARNESSES.filter(name => !(name in HARNESS_DEADLINE_SECONDS))) expect(harnessDeadlineSeconds(harness)).toBe(DEFAULT_DEADLINE_SECONDS);
    expect(DEFAULT_DEADLINE_SECONDS).toBe(60);
    // Every override names a real page, and the WebKit runner is told the number.
    expect(Object.keys(HARNESS_DEADLINE_SECONDS).every(name => BROWSER_HARNESSES.includes(name))).toBe(true);
    const runner = readFileSync(new URL("./browser-regressions.mjs", import.meta.url), "utf8");
    expect(runner).toContain("url, String(seconds + 5)");
    const native = readFileSync(new URL("./webkit-regressions.swift", import.meta.url), "utf8");
    expect(native).toContain("withTimeInterval: deadline");
  });
  it("does not hang on teardown when a grandchild keeps stderr open after the child exits", async () => {
    let grandchildPid = 0;
    const child = spawn(
      process.execPath,
      [
        "-e",
        `const { spawn } = require("node:child_process");
         const g = spawn(process.execPath, ["-e", "setTimeout(() => {}, 5000)"], { stdio: ["ignore", "ignore", 2], detached: true });
         g.unref();
         process.stdout.write(String(g.pid));
         process.exit(0);`,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    child.stdout?.on("data", (d: Buffer) => {
      const n = Number(String(d).trim());
      if (Number.isInteger(n) && n > 1) grandchildPid = n;
    });
    try {
      // The hard cap sits above the budget, so returning on the cap (the hang
      // this guards) cannot pass for returning on the child's exit.
      const budget = wallClockBudget(1_500);
      const t0 = Date.now();
      await waitForProcessClose(child, 50, budget * 2);
      expect(Date.now() - t0).toBeLessThan(budget);
    } finally {
      if (grandchildPid > 1) {
        try { process.kill(grandchildPid, "SIGKILL"); } catch { /* already gone */ }
      }
    }
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

  it("says why a browser runner exited before its verdict, not just its exit code", async () => {
    const { browserExitMessage } = await import("./browser-regressions.mjs");
    expect(browserExitMessage("WebKit", 3, "This Mac has Reduce Motion on, so ...\n"))
      .toBe("WebKit exited before the verdict (3): This Mac has Reduce Motion on, so ...");
    expect(browserExitMessage("WebKitGTK", null, "", "SIGSEGV"))
      .toBe("WebKitGTK exited before the verdict (signal SIGSEGV)");
    expect(browserExitMessage("Chrome", 1, "  \n")).toBe("Chrome exited before the verdict (1)");
  });

  // The macos-26 runner image has Reduce Motion on: WKWebView then matches
  // prefers-reduced-motion and every animation check failed with "0 animate
  // calls" (Frontend macos-latest, f9610b895). The runner refuses such a host
  // with the reason, and CI turns the setting off before the WebKit step.
  it("never measures motion on a host that asked for reduced motion", () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const runner = readFileSync(path.join(here, "webkit-regressions.swift"), "utf8");
    const refusal = runner.indexOf("accessibilityDisplayShouldReduceMotion");
    expect(refusal).toBeGreaterThan(-1);
    expect(refusal).toBeLessThan(runner.indexOf("webview.load("));
    const ci = readFileSync(path.join(here, "..", ".github", "workflows", "ci.yml"), "utf8");
    const off = ci.indexOf("defaults write com.apple.universalaccess reduceMotion -bool false");
    expect(off).toBeGreaterThan(-1);
    expect(off).toBeLessThan(ci.indexOf("run: bun run test:webkit:all"));
  });

  // WebKit matches :focus only in a key window; on the runner a window that is
  // never activated never becomes key, so the focus-edge check in tab-switch
  // saw the resting edge. CI lets the runner take focus; a local run does not.
  it("lets the WebKit runner take focus only when CI asks for it", () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const runner = readFileSync(path.join(here, "webkit-regressions.swift"), "utf8");
    const gate = runner.indexOf('environment["GITPULSE_WEBKIT_TAKE_FOCUS"] == "1"');
    expect(gate).toBeGreaterThan(-1);
    expect(runner.indexOf("makeKeyAndOrderFront")).toBeGreaterThan(gate);
    expect(runner.slice(0, gate)).not.toMatch(/activate\(|makeKey/);
    const ci = readFileSync(path.join(here, "..", ".github", "workflows", "ci.yml"), "utf8");
    const step = ci.slice(ci.indexOf("name: Native Renderer Regressions (WKWebView)"), ci.indexOf("run: bun run test:webkit:all"));
    expect(step).toContain('GITPULSE_WEBKIT_TAKE_FOCUS: "1"');
  });

  it("escapes an annotation so its text cannot end the command early", async () => {
    const { annotation } = await import("./browser-regressions.mjs");
    expect(annotation("Chrome a:b,c", "one\ntwo 50%")).toBe("::error title=Chrome a%3Ab%2Cc::one%0Atwo 50%25");
  });
});

describe("the watchdog only ever signals the Chrome it launched", () => {
  const profile = "/tmp/gitpulse-browser-abc123";
  const own = (pid: number) => ({ pgid: pid, command: `Chrome --user-data-dir=${profile} about:blank` });
  it("names Chrome's group when the pid is still its group leader", async () => {
    const { chromeVictim } = await import("./browser-regressions.mjs");
    expect(chromeVictim(4242, profile, "darwin", 7, own)).toBe(-4242);
    expect(chromeVictim(4242, profile, "win32", 7, own)).toBe(4242);
  });
  it("refuses every id that would reach more than Chrome", async () => {
    const { chromeVictim } = await import("./browser-regressions.mjs");
    // process.kill(-0) is kill(0): the watchdog's own group. -1 is everything.
    for (const id of [0, -0, 1, -1, -2, NaN, Infinity, 1.5, 7]) {
      expect(chromeVictim(id, profile, "darwin", 7, own), `pid ${id}`).toBeNull();
    }
  });
  it("refuses a recycled pid: gone, not a group leader, or not this run's Chrome", async () => {
    const { chromeVictim } = await import("./browser-regressions.mjs");
    expect(chromeVictim(4242, profile, "darwin", 7, () => null)).toBeNull();
    expect(chromeVictim(4242, profile, "darwin", 7, () => ({ pgid: 900, command: `Chrome --user-data-dir=${profile}` }))).toBeNull();
    expect(chromeVictim(4242, profile, "darwin", 7, () => ({ pgid: 4242, command: "/usr/bin/vim notes.txt" }))).toBeNull();
    expect(chromeVictim(4242, "", "darwin", 7, own)).toBeNull();
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
      const { startChrome } = await import(${JSON.stringify(pathToFileURL(runner).href)});
      const child = startChrome(${JSON.stringify(chrome)}, ${JSON.stringify(profile)}, "about:blank");
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
      // Chrome is up before the runner dies.
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
