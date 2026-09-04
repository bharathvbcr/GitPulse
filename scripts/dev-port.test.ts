import { spawn, type ChildProcess } from "node:child_process";
import http from "node:http";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS } from "../src/lib/__tests__/perfBudget";
import {
  DevPortError,
  PREFERRED_DEV_PORT,
  RECLAIM_GRACE_MS,
  attachChildLifetime,
  defaultRepoRoot,
  devCspForPort,
  findFreePort,
  formatResolveMessage,
  isDevServerCommand,
  isInsideRepo,
  isPortFree,
  isTauriDevArgs,
  isTauriHookEnv,
  killPid,
  parseEtimeToMs,
  parseLsofPids,
  parseNetstatPids,
  parseOptionalPort,
  parseReclaimEnv,
  portFromEnv,
  resolveDevPort,
  shouldReclaimListener,
  tauriConfigForPort,
  tryListen,
  withTauriDevUrl,
} from "./dev-port.mjs";

const repoRoot = defaultRepoRoot();
const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const liveChildren: ChildProcess[] = [];

afterEach(() => {
  while (liveChildren.length > 0) {
    const child = liveChildren.pop();
    if (child?.pid && child.exitCode == null) {
      try {
        child.kill("SIGKILL");
      } catch {
        // already gone
      }
    }
  }
});

function mockIo(overrides: Record<string, unknown> = {}) {
  return {
    listListeners: async () => [],
    processCommand: async () => "",
    processCwd: async () => "",
    processElapsedMs: async () => null,
    selfPids: () => new Set([process.pid]),
    kill: async () => {},
    killVerified: async () => {},
    isFree: async () => true,
    waitUntilFree: async () => true,
    findFreePort: async (from: number) => from,
    ...overrides,
  };
}

describe("portFromEnv", () => {
  it("falls back when unset", () => {
    expect(portFromEnv({}, 5173)).toBe(5173);
    expect(parseOptionalPort(undefined)).toBeNull();
    expect(parseOptionalPort("")).toBeNull();
  });

  it("reads a valid GITPULSE_DEV_PORT", () => {
    expect(portFromEnv({ GITPULSE_DEV_PORT: "5180" })).toBe(5180);
  });

  it("rejects non-integer and out-of-range values", () => {
    expect(() => portFromEnv({ GITPULSE_DEV_PORT: "nope" })).toThrow(/1-65535/);
    expect(() => parseOptionalPort("0")).toThrow(/1-65535/);
    expect(() => parseOptionalPort("65536")).toThrow(/1-65535/);
    expect(() => parseOptionalPort("5173.5")).toThrow(/1-65535/);
  });
});

describe("isDevServerCommand", () => {
  it("matches Vite listeners", () => {
    expect(
      isDevServerCommand(
        "node /Users/acme/gitpulse/node_modules/vite/bin/vite.js --port 5173",
      ),
    ).toBe(true);
    expect(isDevServerCommand("node ./node_modules/.bin/vite")).toBe(true);
    expect(isDevServerCommand("vite --host 127.0.0.1")).toBe(true);
  });

  it("does not match unrelated processes", () => {
    expect(isDevServerCommand("node server.js")).toBe(false);
    expect(isDevServerCommand("postgres -D /usr/local/var/postgres")).toBe(false);
    expect(isDevServerCommand("Google Chrome")).toBe(false);
    expect(isDevServerCommand("")).toBe(false);
  });
});

describe("shouldReclaimListener", () => {
  const selfPids = new Set([100]);
  const oldEnough = RECLAIM_GRACE_MS * 10;

  function repoVite(overrides: Record<string, unknown> = {}) {
    return {
      pid: 42,
      command: "node /tmp/gitpulse/node_modules/vite/bin/vite.js",
      cwd: "/tmp/gitpulse",
      repoRoot: "/tmp/gitpulse",
      selfPids,
      ...overrides,
    };
  }

  it("reclaims this repo's leftover Vite once it is older than the grace window", () => {
    expect(
      shouldReclaimListener(repoVite({ startedAgoMs: oldEnough })),
    ).toBe(true);
    expect(
      shouldReclaimListener(repoVite({ startedAgoMs: oldEnough, pid: 100 })),
    ).toBe(false);
  });

  it("spares young Vite so concurrent starts cannot kill each other", () => {
    expect(
      shouldReclaimListener(repoVite({ startedAgoMs: RECLAIM_GRACE_MS - 1 })),
    ).toBe(false);
    expect(shouldReclaimListener(repoVite({ startedAgoMs: 0 }))).toBe(false);
  });

  it("never reclaims when the start time is unknown (fail closed)", () => {
    expect(shouldReclaimListener(repoVite({}))).toBe(false);
    expect(
      shouldReclaimListener(repoVite({ startedAgoMs: null })),
    ).toBe(false);
  });

  it("reclaims regardless of age when reclaimAll is opted in (GITPULSE_RECLAIM=1)", () => {
    expect(shouldReclaimListener(repoVite({ reclaimAll: true }))).toBe(true);
    expect(
      shouldReclaimListener(repoVite({ reclaimAll: true, startedAgoMs: 0 })),
    ).toBe(true);
  });

  it("honors a custom grace period", () => {
    expect(
      shouldReclaimListener(
        repoVite({ startedAgoMs: 5_000, graceMs: 1_000 }),
      ),
    ).toBe(true);
    expect(
      shouldReclaimListener(repoVite({ startedAgoMs: 500, graceMs: 1_000 })),
    ).toBe(false);
  });

  it("leaves foreign Vite and non-Vite listeners alone", () => {
    expect(
      shouldReclaimListener(
        repoVite({
          pid: 42,
          command: "node /other/project/node_modules/vite/bin/vite.js",
          cwd: "/other/project",
          startedAgoMs: oldEnough,
        }),
      ),
    ).toBe(false);
    expect(
      shouldReclaimListener(
        repoVite({
          command: "node scripts/fixtures/hold-port.mjs",
          startedAgoMs: oldEnough,
        }),
      ),
    ).toBe(false);
  });
});

describe("parseEtimeToMs", () => {
  it("parses ps etime shapes and rejects garbage", () => {
    expect(parseEtimeToMs("00:07")).toBe(7_000);
    expect(parseEtimeToMs("01:02")).toBe(62_000);
    expect(parseEtimeToMs("01:00:00")).toBe(3_600_000);
    expect(parseEtimeToMs("2-03:04:05")).toBe(((2 * 24 + 3) * 60 + 4) * 60_000 + 5_000);
    expect(parseEtimeToMs("")).toBeNull();
    expect(parseEtimeToMs("7")).toBeNull();
    expect(parseEtimeToMs("soon")).toBeNull();
  });
});

describe("parseReclaimEnv", () => {
  it("treats GITPULSE_RECLAIM=1 as opt-in and most other things as off", () => {
    expect(parseReclaimEnv({})).toBe(false);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "" })).toBe(false);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "0" })).toBe(false);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "false" })).toBe(false);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "off" })).toBe(false);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "1" })).toBe(true);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "true" })).toBe(true);
    expect(parseReclaimEnv({ GITPULSE_RECLAIM: "yes" })).toBe(true);
  });
});

describe("isInsideRepo", () => {
  it("accepts the repo and nested paths, not siblings", () => {
    expect(isInsideRepo("/tmp/gitpulse", "/tmp/gitpulse")).toBe(true);
    expect(isInsideRepo("/tmp/gitpulse/scripts", "/tmp/gitpulse")).toBe(true);
    expect(isInsideRepo("/tmp/gitpulse-foo", "/tmp/gitpulse")).toBe(false);
    expect(isInsideRepo(undefined, "/tmp/gitpulse")).toBe(false);
  });
});

describe("listener parsers", () => {
  it("parses lsof -t pid lists", () => {
    expect(parseLsofPids("12345\n67890\n")).toEqual([12345, 67890]);
    expect(parseLsofPids("")).toEqual([]);
    expect(parseLsofPids("1\n")).toEqual([]);
  });

  it("parses Windows netstat LISTENING rows for a port", () => {
    const stdout = [
      "  TCP    0.0.0.0:5173           0.0.0.0:0              LISTENING       4412",
      "  TCP    127.0.0.1:5174         0.0.0.0:0              LISTENING       88",
      "  TCP    127.0.0.1:5173         0.0.0.0:0              ESTABLISHED     4412",
    ].join("\n");
    expect(parseNetstatPids(stdout, 5173)).toEqual([4412]);
    expect(parseNetstatPids(stdout, 5174)).toEqual([88]);
  });
});

describe("tauri config helpers", () => {
  it("only injects --config when the port moved", () => {
    expect(withTauriDevUrl(["dev"], 5173)).toEqual(["dev"]);
    expect(withTauriDevUrl(["dev", "--verbose"], 5181)).toEqual([
      "dev",
      "--verbose",
      "--config",
      JSON.stringify(tauriConfigForPort(5181)),
    ]);
    expect(tauriConfigForPort(5181)).toEqual({
      build: { devUrl: "http://localhost:5181" },
      app: { security: { devCsp: devCspForPort(5181) } },
    });
    expect(devCspForPort(5181)["connect-src"]).toContain("ws://localhost:5181");
    expect(devCspForPort(5181)["script-src"]).toContain("http://127.0.0.1:5181");
  });

  it("keeps generated dev CSP in lockstep with tauri.conf.json at the preferred port", () => {
    const conf = JSON.parse(
      readFileSync(path.join(repoRoot, "src-tauri/tauri.conf.json"), "utf8"),
    ) as { app: { security: { devCsp: Record<string, string> } } };
    expect(devCspForPort(PREFERRED_DEV_PORT)).toEqual(conf.app.security.devCsp);
  });

  it("detects the Tauri beforeDevCommand hook env that disables WKWebView HMR", () => {
    expect(isTauriHookEnv({})).toBe(false);
    expect(isTauriHookEnv({ TAURI_ENV_PLATFORM: "darwin" })).toBe(true);
    expect(isTauriHookEnv({ TAURI_ENV_DEBUG: "true" })).toBe(true);
    expect(isTauriHookEnv({ TAURI_ENV_ARCH: "aarch64" })).toBe(true);
  });

  it("forces a full reload in the Tauri webview instead of ESM HMR", () => {
    const source = readFileSync(path.join(repoRoot, "vite.config.ts"), "utf8");
    expect(source).toContain("gitpulse-tauri-full-reload");
    expect(source).toContain("isTauriHookEnv");
    expect(source).toContain('type: "full-reload"');
    expect(source).toContain('order: "pre"');
    expect(source).toContain("hotUpdate:");
    expect(source).toContain("hmr: !isTauriHookEnv()");
  });

  it("detects tauri dev vs help", () => {
    expect(isTauriDevArgs(["dev"])).toBe(true);
    expect(isTauriDevArgs(["dev", "--help"])).toBe(false);
    expect(isTauriDevArgs(["build"])).toBe(false);
  });
});

describe("attachChildLifetime", () => {
  it("forwards SIGTERM to the child and exits when the child exits", () => {
    const killed: string[] = [];
    const listeners = new Map<string, (...args: unknown[]) => void>();
    const childListeners = new Map<string, (...args: unknown[]) => void>();
    const child = {
      exitCode: null as number | null,
      signalCode: null as string | null,
      kill: (signal: string) => {
        killed.push(signal);
      },
      on: (event: string, cb: (...args: unknown[]) => void) => {
        childListeners.set(event, cb);
      },
    };
    const exits: number[] = [];
    const hooks = {
      on: (event: string, cb: (...args: unknown[]) => void) => {
        listeners.set(event, cb);
      },
      removeListener: (event: string) => {
        listeners.delete(event);
      },
      exit: (code: number) => {
        exits.push(code);
      },
    };
    attachChildLifetime(child as never, hooks as never);
    listeners.get("SIGTERM")?.();
    expect(killed).toEqual(["SIGTERM"]);
    childListeners.get("exit")?.(0, null);
    expect(exits).toEqual([0]);
  });
});

describe("resolveDevPort", () => {
  it("uses the preferred port when it is free", async () => {
    const result = await resolveDevPort({
      preferred: 5173,
      env: {},
      repoRoot: "/tmp/gitpulse",
      io: mockIo(),
    });
    expect(result).toEqual({ port: 5173, source: "preferred" });
  });

  it("reclaims this app's leftover Vite instead of hopping ports", async () => {
    const killed: number[] = [];
    const result = await resolveDevPort({
      preferred: 5173,
      env: {},
      repoRoot: "/tmp/gitpulse",
      io: mockIo({
        listListeners: async () => [{ pid: 4242 }],
        processCommand: async () =>
          "node /tmp/gitpulse/node_modules/vite/bin/vite.js",
        processCwd: async () => "/tmp/gitpulse",
        processElapsedMs: async () => RECLAIM_GRACE_MS * 10,
        isFree: async () => false,
        killVerified: async (pid: number, needle: string | null) => {
          killed.push(pid);
          if (needle != null) expect(needle).toContain("vite");
        },
        waitUntilFree: async () => true,
      }),
    });
    expect(killed).toEqual([4242]);
    expect(result.source).toBe("cleaned");
    expect(result.port).toBe(5173);
    expect(formatResolveMessage(result)).toMatch(/Reclaimed port 5173/);
  });

  it("spares a just-started repo Vite so concurrent dev starts cannot kill each other", async () => {
    const killed: number[] = [];
    let elapsedCalls = 0;
    const result = await resolveDevPort({
      preferred: 5173,
      env: {},
      repoRoot: "/tmp/gitpulse",
      allowAutoport: true,
      io: mockIo({
        listListeners: async () => [{ pid: 4242 }],
        processCommand: async () =>
          "node /tmp/gitpulse/node_modules/vite/bin/vite.js",
        processCwd: async () => "/tmp/gitpulse",
        processElapsedMs: async () => {
          elapsedCalls += 1;
          return 1_000; // one second old: someone else's live dev server
        },
        isFree: async () => false,
        killVerified: async (pid: number, needle: string | null) => {
          killed.push(pid);
          if (needle != null) expect(needle).toContain("vite");
        },
        findFreePort: async () => 5174,
      }),
    });
    expect(killed).toEqual([]);
    expect(elapsedCalls).toBe(1);
    expect(result.source).toBe("autoport");
    expect(result.port).toBe(5174);
  });

  it("reclaims even a young Vite when GITPULSE_RECLAIM=1", async () => {
    const killed: number[] = [];
    let elapsedCalls = 0;
    const result = await resolveDevPort({
      preferred: 5173,
      env: { GITPULSE_RECLAIM: "1" },
      repoRoot: "/tmp/gitpulse",
      io: mockIo({
        listListeners: async () => [{ pid: 4242 }],
        processCommand: async () =>
          "node /tmp/gitpulse/node_modules/vite/bin/vite.js",
        processCwd: async () => "/tmp/gitpulse",
        processElapsedMs: async () => {
          elapsedCalls += 1;
          return 0;
        },
        isFree: async () => false,
        killVerified: async (pid: number, needle: string | null) => {
          killed.push(pid);
          if (needle != null) expect(needle).toContain("vite");
        },
        waitUntilFree: async () => true,
      }),
    });
    expect(elapsedCalls).toBe(0); // opt-in skips the age probe entirely
    expect(killed).toEqual([4242]);
    expect(result.source).toBe("cleaned");
  });

  it("auto-selects the next free port when a foreign process owns the preferred one", async () => {
    const result = await resolveDevPort({
      preferred: 5173,
      env: {},
      repoRoot: "/tmp/gitpulse",
      allowAutoport: true,
      io: mockIo({
        listListeners: async () => [{ pid: 88 }],
        processCommand: async () => "ControlCenter",
        processCwd: async () => "/System",
        isFree: async () => false,
        findFreePort: async () => 5174,
      }),
    });
    expect(result).toMatchObject({ port: 5174, source: "autoport" });
    expect(formatResolveMessage(result)).toMatch(/using 5174/);
  });

  it("does not autoport when the port is locked by env or a Tauri hook", async () => {
    await expect(
      resolveDevPort({
        preferred: 5173,
        env: { GITPULSE_DEV_PORT: "5173" },
        repoRoot: "/tmp/gitpulse",
        io: mockIo({
          listListeners: async () => [{ pid: 88 }],
          processCommand: async () => "ControlCenter",
          isFree: async () => false,
        }),
      }),
    ).rejects.toBeInstanceOf(DevPortError);

    await expect(
      resolveDevPort({
        preferred: 5173,
        env: { TAURI_ENV_PLATFORM: "darwin" },
        repoRoot: "/tmp/gitpulse",
        io: mockIo({
          listListeners: async () => [{ pid: 88 }],
          processCommand: async () => "ControlCenter",
          isFree: async () => false,
        }),
      }),
    ).rejects.toThrow(/Port 5173 is already in use/);
  });

  it("honors GITPULSE_DEV_PORT when that port is free", async () => {
    const result = await resolveDevPort({
      preferred: 5173,
      env: { GITPULSE_DEV_PORT: "5190" },
      repoRoot: "/tmp/gitpulse",
      io: mockIo(),
    });
    expect(result).toEqual({ port: 5190, source: "env" });
  });
});

describe("tryListen error matrix", () => {
  it("reports free only when a bind succeeds", async () => {
    const port = await findFreePort(19000, 19900);
    const diagnostics: string[] = [];
    await expect(tryListen(port, "127.0.0.1", diagnostics)).resolves.toBe(true);
    expect(diagnostics).toEqual([]);
  });

  it("reports EADDRINUSE as taken with no diagnostic", async () => {
    const port = await findFreePort(19000, 19900);
    const server = await listenOn(port);
    try {
      const diagnostics: string[] = [];
      await expect(tryListen(port, "127.0.0.1", diagnostics)).resolves.toBe(
        false,
      );
      expect(diagnostics).toEqual([]);
    } finally {
      await closeServer(server);
    }
  });

  it("fails closed for unexpected bind errors and records the code", async () => {
    // Ports above 65535 make Node throw ERR_SOCKET_BAD_PORT synchronously:
    // a deterministic stand-in for EACCES/EADDRNOTAVAIL-class failures.
    const diagnostics: string[] = [];
    await expect(tryListen(70000, undefined, diagnostics)).resolves.toBe(false);
    expect(diagnostics).toHaveLength(1);
    expect(diagnostics[0]).toMatch(/ERR_SOCKET_BAD_PORT/);

    // The old behavior resolved `true` here, silently claiming a port was
    // free when the probe itself had failed.
    await expect(isPortFree(70000)).resolves.toBe(false);
  });

  it("isPortFree surfaces probe failure codes through diagnostics", async () => {
    const diagnostics: string[] = [];
    await expect(isPortFree(70000, diagnostics)).resolves.toBe(false);
    expect(diagnostics.join("; ")).toMatch(/ERR_SOCKET_BAD_PORT/);
  });
});

describe("findFreePort argument handling", () => {
  it("rejects inverted or invalid ranges with a distinct message", async () => {
    await expect(findFreePort(5180, 5173)).rejects.toThrow(/invalid port range/);
    await expect(findFreePort(Number.NaN, 5180)).rejects.toThrow(
      /invalid port range/,
    );
    await expect(findFreePort(0, 10)).rejects.toThrow(/invalid port range/);
    await expect(findFreePort(60000, 70000)).rejects.toThrow(
      /invalid port range/,
    );
  });

  it("throws an honest all-busy message (not 'could not identify') when the range is exhausted", async () => {
    const base = await findFreePort(19000, 19900);
    const first = await listenOn(base);
    const second = await listenOn(base + 1);
    try {
      const err = await findFreePort(base, base + 1).catch((e) => e);
      expect(err).toBeInstanceOf(DevPortError);
      expect(err.message).toMatch(new RegExp(`Ports ${base}-${base + 1} are all busy`));
      expect(err.message).not.toMatch(/could not identify/);
      expect(Array.isArray(err.diagnostics)).toBe(true);
    } finally {
      await closeServer(first);
      await closeServer(second);
    }
  });
});

describe("live ports", () => {
  it("findFreePort skips an occupied bind", async () => {
    const base = await findFreePort(19000, 19900);
    const occupied = await listenOn(base);
    try {
      const next = await findFreePort(base, base + 5);
      expect(next).toBeGreaterThan(base);
      expect(await isPortFree(next)).toBe(true);
    } finally {
      await closeServer(occupied);
    }
  }, STRESS_TIMEOUT_MS);

  it.runIf(process.platform !== "win32")(
    "kills a leftover Vite-named listener in this repo and keeps the preferred port",
    async () => {
      const port = await findFreePort(19000, 19900);
      const child = await spawnHolder(
        path.join(scriptsDir, "fixtures/vite/hold-port.mjs"),
        port,
      );
      expect(await isPortFree(port)).toBe(false);

      const result = await resolveDevPort({
        preferred: port,
        // The holder was spawned milliseconds ago; opting in via
        // GITPULSE_RECLAIM exercises reclaim regardless of the grace window.
        env: { GITPULSE_RECLAIM: "1" },
        repoRoot,
        allowAutoport: true,
      });

      expect(result.port).toBe(port);
      expect(result.source).toBe("cleaned");
      expect(await isPortFree(port)).toBe(true);
      await waitForExit(child);
    },
  );

  it.runIf(process.platform !== "win32")(
    "does not kill a non-Vite holder; picks another port instead",
    async () => {
      const port = await findFreePort(19000, 19900);
      await spawnHolder(path.join(scriptsDir, "fixtures/hold-port.mjs"), port);
      expect(await isPortFree(port)).toBe(false);

      const result = await resolveDevPort({
        preferred: port,
        env: {},
        repoRoot,
        allowAutoport: true,
        range: 20,
      });

      expect(result.source).toBe("autoport");
      expect(result.port).not.toBe(port);
      expect(await isPortFree(port)).toBe(false);
      expect(await isPortFree(result.port)).toBe(true);
    },
  );
});

describe("PREFERRED_DEV_PORT", () => {
  it("stays aligned with src-tauri/tauri.conf.json build.devUrl", () => {
    const confPath = path.join(repoRoot, "src-tauri", "tauri.conf.json");
    const conf = JSON.parse(readFileSync(confPath, "utf8")) as {
      build?: { devUrl?: string };
    };
    const devUrl = conf.build?.devUrl;
    expect(devUrl).toBeDefined();
    expect(devUrl).toMatch(/:5173$/);
    expect(Number(new URL(devUrl ?? "").port)).toBe(PREFERRED_DEV_PORT);
  });
});

function listenOn(port: number): Promise<http.Server> {
  return new Promise((resolve, reject) => {
    const server = http.createServer();
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => resolve(server));
  });
}

function closeServer(server: http.Server): Promise<void> {
  return new Promise((resolve, reject) => {
    server.close((err) => (err ? reject(err) : resolve()));
  });
}

async function spawnHolder(script: string, port: number): Promise<ChildProcess> {
  const child = spawn(process.execPath, [script, String(port)], {
    cwd: repoRoot,
    stdio: ["ignore", "pipe", "pipe"],
  });
  liveChildren.push(child);
  await waitForReady(child);
  return child;
}

function waitForReady(child: ChildProcess): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error("holder did not become ready"));
    }, 3000);
    child.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`holder exited before ready (${code})`));
    });
    child.stdout?.on("data", (chunk: Buffer) => {
      if (chunk.toString().includes("ready")) {
        clearTimeout(timer);
        child.stdout?.removeAllListeners("data");
        child.removeAllListeners("exit");
        resolve();
      }
    });
  });
}

function waitForExit(child: ChildProcess): Promise<void> {
  if (child.exitCode != null) return Promise.resolve();
  return new Promise((resolve) => {
    child.once("exit", () => resolve());
    setTimeout(resolve, 2000);
  });
}

describe("killPid identity check", () => {
  /** Liveness via the same mechanism production killPid trusts. */
  async function aliveViaPs(pid: number): Promise<boolean> {
    const { execFile } = await import("node:child_process");
    const { promisify } = await import("node:util");
    try {
      await promisify(execFile)("ps", ["-p", String(pid), "-o", "command="]);
      return true;
    } catch {
      return false;
    }
  }

  it("kills a process whose command still matches the vetted needle", async () => {
    if (process.platform === "win32") return; // ps-based needle is POSIX-only
    const child = spawn("sleep", ["5"]);
    await killPid(child.pid!, "sleep 5");
    // Poll instead of trusting the 'exit' event, which can lag in test hosts.
    let dead = false;
    for (let i = 0; i < 60 && !dead; i += 1) {
      if (child.exitCode !== null || !(await aliveViaPs(child.pid!))) dead = true;
      else await new Promise((r) => setTimeout(r, 50));
    }
    expect(dead).toBe(true);
  }, 10_000);

  it("refuses to signal when the command no longer matches (pid-reuse guard)", async () => {
    if (process.platform === "win32") return;
    const child = spawn("sleep", ["5"]);
    // A needle that cannot match `sleep` simulates the pid having been
    // recycled by an unrelated process between listing and kill.
    await killPid(child.pid!, "totally-unrelated-process");
    expect(child.exitCode).toBeNull(); // untouched
    expect(await aliveViaPs(child.pid!)).toBe(true);
    child.kill("SIGKILL");
  }, 10_000);
});
