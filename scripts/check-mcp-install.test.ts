import { constants } from "node:fs";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_TIMEOUT_MS,
  HOOK_BIN,
  SERVER_BIN,
  classify,
  classifyHook,
  declaredHookSubcommands,
  parseArgs,
  parseHookIdentity,
  parseServerVersion,
  parseServerManifest,
  probeHook,
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

async function identityServer(prefix: string, schema: number) {
  return fakeServer(prefix, `
    let input = "";
    process.stdin.on("data", chunk => {
      input += chunk;
      let end;
      while ((end = input.indexOf("\\n")) >= 0) {
        const request = JSON.parse(input.slice(0, end));
        input = input.slice(end + 1);
        if (request.id === 1) {
          process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { serverInfo: { name: "fake", version: "1.2.3" } } }) + "\\n");
        } else if (request.id === 2 && request.method === "resources/read") {
          process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 2, result: { contents: [{ uri: "gitpulse://server/manifest", mimeType: "application/json", text: JSON.stringify({ storeSchemaVersion: ${schema} }) }] } }) + "\\n");
        }
      }
    });
  `);
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
    // Node ignores X_OK on Windows, and chmod cannot clear an execute bit
    // there, so a host chmod is not a portable way to stage "not executable".
    const access = (_full: string, mode: number) => {
      if (mode === constants.X_OK) {
        throw Object.assign(new Error("EACCES"), { code: "EACCES" });
      }
    };
    expect(resolveOnPath(SERVER_BIN, { pathValue: dir, platform: "linux", access })).toBeNull();
  });

  const onPosix = it.runIf(process.platform !== "win32");
  onPosix("refuses a mode-0644 file on a Unix host, not only an injected access", async () => {
    const dir = await scratchDir("nonexec-chmod");
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
  const identity = { storeSchema: 20, expectedSchema: 20 };

  it("rejects an older embedded schema even when the application version matches", () => {
    const observed = { binPath: "/x/gitpulse-mcp", version: expected, error: null, expected, storeSchema: 19, expectedSchema: 20 };
    expect(classify(observed).status).toBe("stale");
  });

  it("does not approve a schema check that the server could not answer", () => {
    const observed = { binPath: "/x/gitpulse-mcp", version: expected, error: null, expected, storeSchema: null, expectedSchema: 20 };
    expect(classify(observed).status).not.toBe("ok");
  });

  it("reports ok when both version and schema match", () => {
    expect(classify({ binPath: "/x/gitpulse-mcp", version: "0.0.5", error: null, expected, ...identity }).status).toBe("ok");
  });

  it("separates absent from ok — the collapse this check exists to prevent", () => {
    const absent = classify({ binPath: null, version: null, error: null, expected, ...identity });
    expect(absent.status).toBe("absent");
    expect(absent.violations.join(" ")).toContain("no gitpulse-mcp on PATH");
  });

  it("names both versions when the installed server is stale", () => {
    const stale = classify({ binPath: "/x/gitpulse-mcp", version: "0.0.4", error: null, expected, ...identity });
    expect(stale.status).toBe("stale");
    expect(stale.violations.join(" ")).toContain('"0.0.4"');
    expect(stale.violations.join(" ")).toContain('"0.0.5"');
  });

  it("distinguishes a server that never answered from one that answered wrong", () => {
    const dead = classify({ binPath: "/x/gitpulse-mcp", version: null, error: "boom", expected, ...identity });
    expect(dead.status).toBe("unresponsive");
    expect(dead.violations.join(" ")).toContain("boom");
  });
});

describe("parseServerManifest", () => {
  function response(schema: unknown, mimeType = "application/json", id = 2) {
    return JSON.stringify({ jsonrpc: "2.0", id, result: { contents: [{ uri: "gitpulse://server/manifest", mimeType, text: JSON.stringify({ storeSchemaVersion: schema }) }] } });
  }

  it("requires the requested resource and response id", () => {
    expect(parseServerManifest(response(20), 2)).toEqual({ storeSchema: 20, error: null });
    expect(parseServerManifest(response(20, "application/json", 3), 2)).toBeNull();
    expect(parseServerManifest(response(20).replace("server/manifest", "server/health"), 2)?.storeSchema).toBeNull();
  });

  it.each([null, "20", 0, -1, 0.5, true, [], {}, Number.MAX_SAFE_INTEGER + 1])("refuses invalid schema %j", schema => {
    const result = parseServerManifest(response(schema), 2);
    expect(result?.storeSchema).toBeNull();
    expect(result?.error).toBeTruthy();
  });

  it("distinguishes missing replies, failed replies, and truncated content", () => {
    expect(parseServerManifest("{partial", 2)).toBeNull();
    expect(parseServerManifest(JSON.stringify({ id: 2, error: { code: -32601 } }), 2)?.error).toBeTruthy();
    expect(parseServerManifest(response(20, "text/plain"), 2)?.error).toMatch(/truncated/);
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
  onPosix("reads version and schema through the handshake and manifest", async () => {
    const server = await identityServer("ok", 20);
    const result = await probeServer(server, 8000);
    expect(result).toEqual({ version: "1.2.3", storeSchema: 20, error: null });
  });

  onPosix("observes an older schema instead of inferring it from the version", async () => {
    const result = await probeServer(await identityServer("older", 19), 8000);
    expect(result).toEqual({ version: "1.2.3", storeSchema: 19, error: null });
  });

  onPosix("bounds a server that floods stdout without a response", async () => {
    const server = await fakeServer("flood", `process.stdout.write("x".repeat(2_000_000)); setInterval(() => {}, 1000);`);
    const result = await probeServer(server, 8000);
    expect(result.storeSchema).toBeNull();
    expect(result.error).toMatch(/exceeded .* bytes/);
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

/** An executable stand-in for `gitpulse-hook --version`. */
async function fakeHook(prefix: string, stdout: string) {
  const dir = await scratchDir(prefix);
  const file = path.join(dir, "fake-hook.mjs");
  await writeFile(file, `#!/usr/bin/env node\nprocess.stdout.write(${JSON.stringify(stdout)});\n`);
  await chmod(file, 0o755);
  return file;
}

describe("parseHookIdentity", () => {
  it("reads the version and the subcommand list", () => {
    expect(parseHookIdentity("gitpulse-hook 1.0.1\nsubcommands: a, b, c\n")).toEqual({
      version: "1.0.1",
      subcommands: ["a", "b", "c"],
    });
  });

  it("survives log noise on stderr-shaped lines without throwing", () => {
    expect(
      parseHookIdentity("warning: something\ngitpulse-hook 2.0.0\nsubcommands: only-one\n"),
    ).toEqual({ version: "2.0.0", subcommands: ["only-one"] });
  });

  it("keeps a half-answer half, rather than inventing the missing side", () => {
    // A binary that printed a version and no subcommands has not identified
    // itself; defaulting the list to empty would read as "serves nothing" and
    // defaulting it to the declared set would read as a pass.
    expect(parseHookIdentity("gitpulse-hook 1.0.1\n")).toEqual({ version: "1.0.1", subcommands: null });
    expect(parseHookIdentity("subcommands: a\n")).toEqual({ version: null, subcommands: ["a"] });
    expect(parseHookIdentity("")).toEqual({ version: null, subcommands: null });
  });

  it("does not mistake another binary's version line for ours", () => {
    expect(parseHookIdentity("gitpulse-mcp 1.0.1\n").version).toBeNull();
  });
});

describe("declaredHookSubcommands", () => {
  it("derives the subcommands from the shipped manifest", () => {
    const declared = declaredHookSubcommands();
    expect(declared.length).toBeGreaterThan(0);
    expect(declared).toEqual([...declared].sort());
    // Spelled out once, so a manifest that silently loses a hook is caught
    // here as well as by the plugin contract.
    expect(declared).toContain("collision-guard");
    expect(declared).toContain("command-gate");
    expect(declared).toContain("session-brief");
  });

  it("throws rather than returning an empty list when the manifest is unreadable", async () => {
    // "We could not find out what the manifest declares" must never be served
    // as "it declares nothing", which would make every binary pass.
    const dir = await scratchDir("no-manifest");
    expect(() => declaredHookSubcommands(dir)).toThrow();
  });
});

describe("classifyHook", () => {
  const base = { expected: "1.0.1", declared: ["collision-guard", "command-gate"], error: null };

  it("separates absent from ok — the collapse that shipped a dead gate", () => {
    const absent = classifyHook({ ...base, binPath: null, version: null, subcommands: null });
    expect(absent.status).toBe("absent");
    expect(absent.violations.join(" ")).toContain("silently disabled");

    expect(
      classifyHook({ ...base, binPath: "/usr/bin/gitpulse-hook", version: "1.0.1", subcommands: ["collision-guard", "command-gate"] }).status,
    ).toBe("ok");
  });

  it("reports a binary that answered nothing as unresponsive, not absent", () => {
    expect(
      classifyHook({ ...base, binPath: "/usr/bin/gitpulse-hook", version: null, subcommands: null, error: "no identity" }).status,
    ).toBe("unresponsive");
  });

  it("names both versions when the installed hook is stale", () => {
    const stale = classifyHook({ ...base, binPath: "/usr/bin/gitpulse-hook", version: "0.9.0", subcommands: ["collision-guard", "command-gate"] });
    expect(stale.status).toBe("stale");
    expect(stale.violations[0]).toContain("0.9.0");
    expect(stale.violations[0]).toContain("1.0.1");
  });

  it("refuses a current binary that cannot serve a declared hook", () => {
    // The drift a source-only contract test cannot see: the manifest a client
    // reads asks for a subcommand the executable on PATH does not implement,
    // and an unknown subcommand answers with silence by design.
    const drifted = classifyHook({ ...base, binPath: "/usr/bin/gitpulse-hook", version: "1.0.1", subcommands: ["collision-guard"] });
    expect(drifted.status).toBe("stale");
    expect(drifted.violations[0]).toContain("command-gate");
  });

  it("ignores extra subcommands the manifest does not ask for", () => {
    expect(
      classifyHook({ ...base, binPath: "/usr/bin/gitpulse-hook", version: "1.0.1", subcommands: ["collision-guard", "command-gate", "future-hook"] }).status,
    ).toBe("ok");
  });
});

/**
 * A shebang script is the only executable a test can synthesise in process,
 * and it is POSIX-only: Windows dispatches on the extension, `CreateProcess`
 * cannot run a `.mjs`, and `spawn` without a shell refuses a `.cmd` — so
 * `fakeHook` there yields `spawn EFTYPE` rather than the behaviour under test.
 * The shipped Windows hook is `gitpulse-hook.exe`, a real executable, so the
 * production path is not what these skip; only the stand-in is. The
 * spawn-failure branch stays covered on every platform by the missing-binary
 * case below, and the parsing itself by the `parseHookIdentity` suite.
 */
const itPosix = it.skipIf(process.platform === "win32");

describe("probeHook", () => {
  itPosix("reads a complete identity split across writes", async () => {
    const bin = await fakeHook("ok", "gitpulse-hook 1.0.1\nsubcommands: collision-guard, command-gate, session-brief\n");
    await expect(probeHook(bin, 5000)).resolves.toEqual({
      version: "1.0.1",
      subcommands: ["collision-guard", "command-gate", "session-brief"],
      error: null,
    });
  });

  itPosix("reports a binary that prints nothing rather than hanging on it", async () => {
    const bin = await fakeHook("silent", "");
    const result = await probeHook(bin, 5000);
    expect(result.version).toBeNull();
    expect(result.error).toContain("identity");
  });

  it("reports a missing executable instead of throwing", async () => {
    const dir = await scratchDir("missing");
    const result = await probeHook(path.join(dir, "not-here"), 5000);
    expect(result.version).toBeNull();
    expect(result.error).toBeTruthy();
  });

  itPosix("bounds a binary that never exits", async () => {
    // The identity path reads no stdin, so a build that blocks on it would
    // hang this probe forever without the deadline.
    const dir = await scratchDir("hang");
    const file = path.join(dir, "hang.mjs");
    await writeFile(file, "#!/usr/bin/env node\nsetInterval(() => {}, 1000);\n");
    await chmod(file, 0o755);
    const started = Date.now();
    const result = await probeHook(file, 400);
    expect(result.error).toContain("within 400ms");
    expect(Date.now() - started).toBeLessThan(5000);
  });
});

describe("the doctor covers both executables the package spawns", () => {
  it("names the hook binary, not only the server", () => {
    expect(SERVER_BIN).toBe("gitpulse-mcp");
    expect(HOOK_BIN).toBe("gitpulse-hook");
  });

  it("accepts a hook binary path so the probe is drivable without an install", () => {
    expect(parseArgs(["--hook-bin", "/tmp/h"]).hookBin).toBe(path.resolve("/tmp/h"));
  });
});
