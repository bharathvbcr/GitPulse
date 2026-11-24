import { describe, expect, it } from "vitest";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import {
  WebDriverClient,
  buildIsolatedEnv,
  buildPtyProbeCommand,
  decodeAndVerifyPng,
  defaultAppBinary,
  defaultSeedBinary,
  detectDisplayBackend,
  main,
  parseArgs,
  remainingBudgetMs,
} from "./e2e-driver-smoke.mjs";

const TINY_PNG_BASE64 = Buffer.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
  0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
]).toString("base64");

describe("e2e-driver-smoke", () => {
  it("answers --help with exit 0 and refuses unknown flags with exit 2", async () => {
    const logged: string[] = [];
    const origLog = console.log;
    const origErr = console.error;
    console.log = (...a: unknown[]) => void logged.push(a.join(" "));
    console.error = () => {};
    try {
      expect(await main(["--help"])).toBe(0);
      expect(logged.join("\n")).toContain("Usage:");
      expect(await main(["--bogus-flag"])).toBe(2);
    } finally {
      console.log = origLog;
      console.error = origErr;
    }
  });

  it("isolates config, data, cache, gitconfig, and WebView2 profile per OS", () => {
    const linux = buildIsolatedEnv("/tmp/e2e-p", { PATH: "/usr/bin" }, "linux");
    expect(linux.HOME).toBe(path.join("/tmp/e2e-p", "home"));
    expect(linux.XDG_CONFIG_HOME).toBe(path.join("/tmp/e2e-p", "config"));
    expect(linux.XDG_DATA_HOME).toBe(path.join("/tmp/e2e-p", "data"));
    expect(linux.XDG_CACHE_HOME).toBe(path.join("/tmp/e2e-p", "cache"));
    expect(linux.GIT_CONFIG_NOSYSTEM).toBe("1");
    expect(linux.WEBKIT_DISABLE_DMABUF_RENDERER).toBe("1");

    const win = buildIsolatedEnv("C:\\temp\\e2e-p", { PATH: "C:\\Windows" }, "win32");
    expect(win.USERPROFILE).toBe(path.join("C:\\temp\\e2e-p", "home"));
    expect(win.APPDATA).toBe(path.join("C:\\temp\\e2e-p", "config"));
    expect(win.LOCALAPPDATA).toBe(path.join("C:\\temp\\e2e-p", "data"));
    expect(win.WEBVIEW2_USER_DATA_FOLDER).toBe(path.join("C:\\temp\\e2e-p", "webview2"));
  });

  it("detects display backend accurately across X11, Wayland, Windows, and headless Linux", () => {
    expect(detectDisplayBackend("linux", { DISPLAY: ":99" })).toBe("x11");
    expect(detectDisplayBackend("linux", { WAYLAND_DISPLAY: "wayland-0" })).toBe("wayland");
    expect(detectDisplayBackend("linux", { DISPLAY: ":99", WAYLAND_DISPLAY: "wayland-0", GDK_BACKEND: "x11" })).toBe("x11");
    expect(detectDisplayBackend("linux", { DISPLAY: ":99", WAYLAND_DISPLAY: "wayland-0", GDK_BACKEND: "wayland" })).toBe("wayland");
    expect(detectDisplayBackend("linux", {})).toBe("none");
    expect(detectDisplayBackend("win32", {})).toBe("win32");
  });

  it("validates PNG magic bytes and refuses corrupt or truncated screenshots", () => {
    const decoded = decodeAndVerifyPng(TINY_PNG_BASE64);
    expect(decoded[0]).toBe(0x89);
    expect(decoded[1]).toBe(0x50);
    expect(() => decodeAndVerifyPng("")).toThrow(/empty|truncated/);
    expect(() => decodeAndVerifyPng(Buffer.from("not-a-png-at-all-definitely-invalid-bytes").toString("base64"))).toThrow(/valid PNG/);
  });

  it("resolves default binaries honoring CARGO_TARGET_DIR and rejects bad ports/timeouts", () => {
    expect(defaultAppBinary("linux", {}, "/repo")).toBe(path.join("/repo", "src-tauri", "target", "debug", "gitpulse"));
    expect(defaultAppBinary("win32", {}, "/repo")).toBe(path.join("/repo", "src-tauri", "target", "debug", "gitpulse.exe"));
    expect(defaultSeedBinary("linux", { CARGO_TARGET_DIR: "/target" }, "/repo")).toBe(path.join("/target", "debug", "examples", "e2e_seed_trust"));
    expect(() => parseArgs(["--port", "0"])).toThrow(/1\.\.65535/);
    expect(() => parseArgs(["--timeout", "5"])).toThrow(/30 and 900/);
  });

  it("speaks W3C WebDriver HTTP for session creation, script execution, screenshots, and teardown", async () => {
    const seen: Array<{ method: string; url: string; body: unknown }> = [];
    const fakeFetch: typeof fetch = async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      seen.push({ method, url, body });
      if (method === "POST" && url.endsWith("/session")) {
        return new Response(JSON.stringify({ value: { sessionId: "sess-123" } }), { status: 200 });
      }
      if (method === "GET" && url.endsWith("/screenshot")) {
        return new Response(JSON.stringify({ value: TINY_PNG_BASE64 }), { status: 200 });
      }
      return new Response(JSON.stringify({ value: { ok: true } }), { status: 200 });
    };

    const client = new WebDriverClient("http://127.0.0.1:4444", fakeFetch);
    expect(await client.createSession("/bin/gitpulse", ["/tmp/repo"])).toBe("sess-123");
    expect(await client.executeScript("return 1", [])).toEqual({ ok: true });
    const dir = mkdtempSync(path.join(tmpdir(), "gp-e2e-unit-"));
    try {
      const png = path.join(dir, "shot.png");
      const info = await client.captureScreenshot(png);
      expect(info.bytes).toBeGreaterThan(8);
      expect(readFileSync(png).subarray(0, 4)).toEqual(Buffer.from([0x89, 0x50, 0x4e, 0x47]));
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
    await client.deleteSession();
    expect(seen.map((s) => `${s.method} ${new URL(s.url).pathname}`)).toEqual([
      "POST /session",
      "POST /session/sess-123/timeouts",
      "POST /session/sess-123/execute/sync",
      "GET /session/sess-123/screenshot",
      "DELETE /session/sess-123",
    ]);
  });

  it("enforces the overall --timeout budget on WebDriver requests", async () => {
    expect(remainingBudgetMs(10_000, 5_000, 2_000)).toBe(5_000);
    expect(remainingBudgetMs(10_000, 5_000, 8_500)).toBe(1_500);
    expect(() => remainingBudgetMs(10_000, 5_000, 10_000)).toThrow(/overall timeout exceeded/i);
    const expiredClient = new WebDriverClient("http://127.0.0.1:4444", fetch, Date.now() - 10);
    await expect(expiredClient.request("GET", "/status")).rejects.toThrow(/overall timeout exceeded/i);
  });

  it("builds PTY/ConPTY probe commands that cannot match local echo alone", () => {
    const prefix = "GITPULSE_E2E_PTY_";
    const suffix = "a1b2c3d4";
    const marker = `${prefix}${suffix}`;
    const unix = buildPtyProbeCommand(prefix, suffix, "linux", {});
    expect(unix.join("")).not.toContain(marker);
    expect(unix[0].endsWith("\n")).toBe(true);

    const winCmd = buildPtyProbeCommand(prefix, suffix, "win32", { COMSPEC: "C:\\Windows\\System32\\cmd.exe" });
    expect(winCmd.join("")).not.toContain(marker);
    expect(winCmd.every((line) => line.endsWith("\r") && !line.endsWith("\r\n"))).toBe(true);

    const winPs = buildPtyProbeCommand(prefix, suffix, "win32", { COMSPEC: "powershell.exe" });
    expect(winPs.join("")).not.toContain(marker);
    expect(winPs[0].endsWith("\r") && !winPs[0].endsWith("\r\n")).toBe(true);
  });

  it("matches every CLI invocation in .github/workflows/e2e-desktop.yml", () => {
    const workflow = readFileSync(new URL("../.github/workflows/e2e-desktop.yml", import.meta.url), "utf8");
    const calls = [...workflow.matchAll(/node scripts\/e2e-driver-smoke\.mjs([^\n]*)/g)];
    expect(calls.length).toBeGreaterThanOrEqual(3);
    for (const match of calls) {
      const tokens = match[1].trim().split(/\s+/).filter(Boolean);
      expect(() => parseArgs(tokens)).not.toThrow();
    }
  });
});
