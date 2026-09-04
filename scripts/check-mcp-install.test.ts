import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_TIMEOUT_MS,
  SERVER_BIN,
  classify,
  parseArgs,
  parseServerVersion,
  probeServer,
  resolveOnPath,
} from "./check-mcp-install.mjs";

const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

async function scratchDir(prefix: string) {
  const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-mcpdoc-${prefix}-`));
  tempDirs.push(dir);
  return dir;
}

/** Write an executable stand-in server so probeServer can be driven without cargo. */
async function fakeServer(prefix: string, body: string) {
  const dir = await scratchDir(prefix);
  const file = path.join(dir, "fake-server.mjs");
  await writeFile(file, `#!/usr/bin/env node\n${body}\n`);
  await chmod(file, 0o755);
  return file;
}

describe("resolveOnPath", () => {
  it("returns the first executable match and ignores earlier non-matches", async () => {
    const empty = await scratchDir("empty");
    const withBin = await scratchDir("withbin");
    const bin = path.join(withBin, SERVER_BIN);
    await writeFile(bin, "#!/bin/sh\nexit 0\n");
    await chmod(bin, 0o755);
    const found = resolveOnPath(SERVER_BIN, {
      pathValue: [empty, withBin].join(path.delimiter),
      platform: "linux",
    });
    expect(found).toBe(bin);
  });

  it("returns null rather than a near-miss when the file is not executable", async () => {
    const dir = await scratchDir("nonexec");
    const bin = path.join(dir, SERVER_BIN);
    await writeFile(bin, "not a program");
    await chmod(bin, 0o644);
    expect(resolveOnPath(SERVER_BIN, { pathValue: dir, platform: "linux" })).toBeNull();
  });

  it("ignores empty PATH segments instead of probing the working directory", () => {
    expect(resolveOnPath(SERVER_BIN, { pathValue: "::", platform: "linux" })).toBeNull();
  });
});

describe("parseServerVersion", () => {
  it("reads serverInfo.version from the response carrying our id", () => {
    const stdout = [
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/message", params: {} }),
      JSON.stringify({ jsonrpc: "2.0", id: 1, result: { serverInfo: { name: "x", version: "0.0.5" } } }),
    ].join("\n");
    expect(parseServerVersion(stdout, 1)).toBe("0.0.5");
  });

  it("ignores a version on a different id", () => {
    // A server answering some other request must not be mistaken for ours.
    const stdout = JSON.stringify({ jsonrpc: "2.0", id: 7, result: { serverInfo: { version: "9.9.9" } } });
    expect(parseServerVersion(stdout, 1)).toBeNull();
  });

  it("survives log noise and partial lines without throwing", () => {
    const stdout = ["INFO starting up", "{not json", ""].join("\n");
    expect(parseServerVersion(stdout, 1)).toBeNull();
  });

  it("returns null when the response has no version rather than inventing one", () => {
    const stdout = JSON.stringify({ jsonrpc: "2.0", id: 1, result: { serverInfo: { name: "x" } } });
    expect(parseServerVersion(stdout, 1)).toBeNull();
  });
});

describe("classify", () => {
  const expected = "0.0.5";

  it("reports ok only when the reported version matches", () => {
    expect(classify({ binPath: "/x/gitpulse-mcp", version: "0.0.5", error: null, expected }).status).toBe("ok");
  });

  it("separates absent from ok — the collapse this check exists to prevent", () => {
    const absent = classify({ binPath: null, version: null, error: null, expected });
    expect(absent.status).toBe("absent");
    expect(absent.violations.join(" ")).toContain("no gitpulse-mcp on PATH");
  });

  it("names both versions when the installed server is stale", () => {
    const stale = classify({ binPath: "/x/gitpulse-mcp", version: "0.0.4", error: null, expected });
    expect(stale.status).toBe("stale");
    expect(stale.violations.join(" ")).toContain('"0.0.4"');
    expect(stale.violations.join(" ")).toContain('"0.0.5"');
  });

  it("distinguishes a server that never answered from one that answered wrong", () => {
    const dead = classify({ binPath: "/x/gitpulse-mcp", version: null, error: "boom", expected });
    expect(dead.status).toBe("unresponsive");
    expect(dead.violations.join(" ")).toContain("boom");
  });
});

/**
 * probeServer spawns an executable directly, so these drive it through a
 * shebang script. Windows cannot exec a `.mjs` that way and `npm test` runs on
 * `windows-latest`, so the spawn-backed cases are POSIX-only — following
 * dev-port.test.ts. The parsing and classification above, which is where the
 * decisions live, still runs on every platform.
 */
const onPosix = it.runIf(process.platform !== "win32");

describe("probeServer", () => {
  onPosix("reads the version from a server that completes the handshake", async () => {
    const server = await fakeServer(
      "ok",
      `process.stdin.once("data", () => {
         process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { serverInfo: { name: "fake", version: "1.2.3" } } }) + "\\n");
       });`,
    );
    const result = await probeServer(server, 8000);
    expect(result).toEqual({ version: "1.2.3", error: null });
  });

  onPosix("times out on a server that accepts input and never answers", async () => {
    const server = await fakeServer("hang", `setInterval(() => {}, 1000);`);
    const result = await probeServer(server, 700);
    expect(result.version).toBeNull();
    expect(result.error).toMatch(/within 700ms/);
  });

  onPosix("reports a server that exits without answering, rather than hanging", async () => {
    const server = await fakeServer("exit", `process.exit(3);`);
    const result = await probeServer(server, 8000);
    expect(result.version).toBeNull();
    expect(result.error).toMatch(/exited without a usable initialize response/);
  });

  onPosix("reports a spawn failure instead of throwing", async () => {
    const result = await probeServer(path.join(await scratchDir("missing"), "nope"), 5000);
    expect(result.version).toBeNull();
    expect(result.error).toBeTruthy();
  });
});

describe("parseArgs", () => {
  it("defaults the timeout and accepts the documented flags", () => {
    expect(parseArgs([]).timeoutMs).toBe(DEFAULT_TIMEOUT_MS);
    const opts = parseArgs(["--expect", "1.2.3", "--timeout", "250", "--json"]);
    expect(opts.expect).toBe("1.2.3");
    expect(opts.timeoutMs).toBe(250);
    expect(opts.json).toBe(true);
  });

  it("refuses a timeout that would disable the bound", () => {
    // An unbounded or nonsense budget turns a hung server into a hung release.
    expect(() => parseArgs(["--timeout", "0"])).toThrow(/positive number/);
    expect(() => parseArgs(["--timeout", "later"])).toThrow(/positive number/);
  });

  it("refuses a flag with no value, and an unknown flag", () => {
    expect(() => parseArgs(["--expect"])).toThrow(/requires a value/);
    expect(() => parseArgs(["--nope"])).toThrow(/unknown argument: --nope/);
  });
});
