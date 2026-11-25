import { constants } from "node:fs";
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_TIMEOUT_MS,
  HOOK_BIN,
  SERVER_BIN,
  classify,
  classifyHook,
  classifyLease,
  formatReport,
  probeLease,
  classifyPluginCache,
  declaredHookSubcommands,
  parseArgs,
  parseHookIdentity,
  parseServerVersion,
  parseServerManifest,
  PLUGIN_ID,
  probeHook,
  probeServer,
  probeServerCommands,
  SERVER_COMMANDS,
  readInstalledPluginHooks,
  readRepoHooks,
  resolveOnPath,
} from "./check-mcp-install.mjs";
import { wallClockBudget } from "../src/lib/testing/wallClock";

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

async function identityServer(prefix: string, schema: number, extra: Record<string, unknown> = {}) {
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
          process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 2, result: { contents: [{ uri: "gitpulse://server/manifest", mimeType: "application/json", text: JSON.stringify({ storeSchemaVersion: ${schema}, ...${JSON.stringify(extra)} }) }] } }) + "\\n");
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

  it("reports ok when version, schema and the one-shot commands match", () => {
    const commands = [...SERVER_COMMANDS];
    expect(classify({ binPath: "/x/gitpulse-mcp", version: "0.0.5", error: null, expected, ...identity, commands }).status).toBe("ok");
  });

  it("marks a server that lists no one-shot commands stale, not ok", () => {
    // An install from before the commands prints the same version and schema.
    for (const commands of [null, undefined]) {
      const old = classify({ binPath: "/x/gitpulse-mcp", version: expected, error: null, expected, ...identity, commands });
      expect(old.status).toBe("stale");
      expect(old.violations.join(" ")).toContain("`gitpulse-mcp call`");
      expect(old.violations.join(" ")).toContain("bun run mcp:install");
    }
  });

  it("names the one-shot command an installed server lacks", () => {
    const partial = classify({ binPath: "/x/gitpulse-mcp", version: expected, error: null, expected, ...identity, commands: ["call", "describe", "tools"] });
    expect(partial.status).toBe("stale");
    expect(partial.violations.join(" ")).toContain("`gitpulse-mcp read`");
    expect(partial.violations.join(" ")).not.toContain("`gitpulse-mcp call`");
  });

  it("does not call a --version that never answered stale", () => {
    const silent = classify({ binPath: "/x/gitpulse-mcp", version: expected, error: null, expected, ...identity, commands: null, commandsError: "no identity response within 10ms" });
    expect(silent.status).toBe("unresponsive");
    expect(silent.violations.join(" ")).toContain("within 10ms");
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
  function response(schema: unknown, mimeType = "application/json", id = 2, extra: Record<string, unknown> = {}) {
    return JSON.stringify({ jsonrpc: "2.0", id, result: { contents: [{ uri: "gitpulse://server/manifest", mimeType, text: JSON.stringify({ storeSchemaVersion: schema, ...extra }) }] } });
  }

  it("requires the requested resource and response id", () => {
    expect(parseServerManifest(response(20), 2)).toEqual({ storeSchema: 20, dcStoreSchema: null, workbenchSchema: null, extractSchema: null, leaseSchema: null, error: null });
    expect(parseServerManifest(response(20, "application/json", 3), 2)).toBeNull();
    expect(parseServerManifest(response(20).replace("server/manifest", "server/health"), 2)?.storeSchema).toBeNull();
  });

  it.each([null, "20", 0, -1, 0.5, true, [], {}, Number.MAX_SAFE_INTEGER + 1])("refuses invalid schema %j", schema => {
    const result = parseServerManifest(response(schema), 2);
    expect(result?.storeSchema).toBeNull();
    expect(result?.error).toBeTruthy();
  });

  it("reads the dc-store and workbench schemas a newer server declares", () => {
    const declared = response(27, "application/json", 2, { dcStoreSchemaVersion: 9, workbenchSchemaVersion: 13, extractionSchemaVersion: 94, stackLeaseFormatVersion: 2 });
    expect(parseServerManifest(declared, 2)).toEqual({ storeSchema: 27, dcStoreSchema: 9, workbenchSchema: 13, extractSchema: 94, leaseSchema: 2, error: null });
  });

  it("reports an older server's missing or invalid dc-store schemas as unknown, never as a number", () => {
    const invalid = response(27, "application/json", 2, { dcStoreSchemaVersion: 0, workbenchSchemaVersion: "13", extractionSchemaVersion: "94" });
    expect(parseServerManifest(invalid, 2)).toEqual({ storeSchema: 27, dcStoreSchema: null, workbenchSchema: null, extractSchema: null, leaseSchema: null, error: null });
  });

  it.each([-1, 0, 94.5, "94", null, true, Number.MAX_SAFE_INTEGER + 1])("reports extractionSchemaVersion %j as unknown, never as a number", extract => {
    const result = parseServerManifest(response(27, "application/json", 2, { extractionSchemaVersion: extract }), 2);
    expect(result?.extractSchema).toBeNull();
    expect(result?.storeSchema).toBe(27);
  });

  it("distinguishes missing replies, failed replies, and truncated content", () => {
    expect(parseServerManifest("{partial", 2)).toBeNull();
    expect(parseServerManifest(JSON.stringify({ id: 2, error: { code: -32601 } }), 2)?.error).toBeTruthy();
    expect(parseServerManifest(response(20, "text/plain"), 2)?.error).toMatch(/truncated/);
  });
});

/**
 * probeServer spawns an executable directly, so these drive it through a
 * shebang script. Windows cannot exec a `.mjs` that way and `bun run test` runs on
 * `windows-latest`, so the spawn-backed cases are POSIX-only — following
 * dev-port.test.ts. The parsing and classification above, which is where the
 * decisions live, still runs on every platform.
 */
const onPosix = it.runIf(process.platform !== "win32");

describe("probeServer", () => {
  onPosix("reads version and schema through the handshake and manifest", async () => {
    const server = await identityServer("ok", 20, { dcStoreSchemaVersion: 9, workbenchSchemaVersion: 13, extractionSchemaVersion: 94, stackLeaseFormatVersion: 2 });
    const result = await probeServer(server, 8000);
    expect(result).toEqual({ version: "1.2.3", storeSchema: 20, dcStoreSchema: 9, workbenchSchema: 13, extractSchema: 94, leaseSchema: 2, error: null });
  });

  onPosix("observes an older schema instead of inferring it from the version", async () => {
    const result = await probeServer(await identityServer("older", 19), 8000);
    expect(result).toEqual({ version: "1.2.3", storeSchema: 19, dcStoreSchema: null, workbenchSchema: null, extractSchema: null, leaseSchema: null, error: null });
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
    expect(declared).toContain("peer-inbox");
    expect(declared).toContain("stop-or-notify");
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
    expect(Date.now() - started).toBeLessThan(wallClockBudget(5000));
  });
});

describe("probeServerCommands", () => {
  itPosix("reads the commands line, sorted", async () => {
    const bin = await fakeHook("cmds", "gitpulse-mcp 1.2.3\ncommands: tools, describe, call, read\n");
    await expect(probeServerCommands(bin, 5000)).resolves.toEqual({
      commands: ["call", "describe", "read", "tools"],
      error: null,
    });
  });

  itPosix("reports an older build's version-only answer as no commands, not an error", async () => {
    const bin = await fakeHook("old", "gitpulse-mcp 1.2.3\n");
    await expect(probeServerCommands(bin, 5000)).resolves.toEqual({ commands: null, error: null });
  });

  it("reports a missing executable as an error", async () => {
    const dir = await scratchDir("missing-server");
    const result = await probeServerCommands(path.join(dir, "not-here"), 5000);
    expect(result.commands).toBeNull();
    expect(result.error).toBeTruthy();
  });
});

describe("SERVER_COMMANDS", () => {
  it("is the CLI_COMMANDS gitpulse-mcp declares", async () => {
    const source = await readFile(new URL("../src-tauri/src/bin/gitpulse-mcp.rs", import.meta.url), "utf8");
    const declared = /const CLI_COMMANDS: \[&str; \d+\] = \[([^\]]*)\];/.exec(source);
    expect(declared, "CLI_COMMANDS not found in gitpulse-mcp.rs").not.toBeNull();
    const names = [...(declared?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();
    expect(names).toEqual([...SERVER_COMMANDS].sort());
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


describe("the installed plugin cache", () => {
  const shipped = {
    hooks: {
      PreToolUse: [{ matcher: "^Bash$", hooks: [{ type: "command", command: "gitpulse-hook command-gate", timeout: 10 }] }],
      SessionStart: [{ hooks: [{ type: "command", command: "gitpulse-hook session-brief", timeout: 10 }] }],
      PostToolUse: [{ hooks: [{ type: "command", command: "gitpulse-hook notify post_tool_use", async: true }] }],
      Stop: [{ hooks: [{ type: "command", command: "gitpulse-hook notify stop", async: true }] }],
    },
  };
  const install = (hooks: unknown, installPath = "/cache/gitpulse/gitpulse/1.4.0") => ({
    registry: "ok",
    registryError: null,
    installs: [{ installPath, hooks, error: null }],
  });

  it("is ok when the installed copy declares what this tree does, however its keys are ordered", () => {
    const reordered = JSON.parse(JSON.stringify(shipped), (_key, value) =>
      value && typeof value === "object" && !Array.isArray(value)
        ? Object.fromEntries(Object.entries(value).reverse())
        : value,
    );
    const verdict = classifyPluginCache(install(reordered), shipped);
    expect(verdict.status).toBe("ok");
    expect(verdict.violations).toEqual([]);
  });

  it("names the hooks the installed copy lacks, the ones declared differently, and the ones only it has", () => {
    const stale = {
      hooks: {
        PreToolUse: [{ matcher: "^Bash$", hooks: [{ type: "command", command: "gitpulse-hook command-gate", timeout: 5 }] }],
        SessionStart: shipped.hooks.SessionStart,
        Elicitation: [{ hooks: [{ type: "command", command: "gitpulse-hook notify elicitation" }] }],
      },
    };
    const verdict = classifyPluginCache(install(stale), shipped);
    expect(verdict.status).toBe("drift");
    const text = verdict.violations.join("\n");
    expect(text).toContain("missing from the installed cache");
    expect(text).toMatch(/missing from the installed cache[^\n]*PostToolUse, Stop/);
    expect(text).toMatch(/declared differently: PreToolUse/);
    expect(text).toMatch(/no longer declared here: Elicitation/);
    expect(text).toContain("never edits the cache");
  });

  it("reports one drifted copy among several, and says nothing was compared when it cannot read one", () => {
    const two = {
      registry: "ok",
      registryError: null,
      installs: [
        { installPath: "/c/1.3.0", hooks: shipped, error: null },
        { installPath: "/c/1.4.0", hooks: { hooks: {} }, error: null },
      ],
    };
    const verdict = classifyPluginCache(two, shipped);
    expect(verdict.status).toBe("drift");
    expect(verdict.violations.join("\n")).toContain("/c/1.4.0/hooks/hooks.json differs");
    expect(verdict.violations.join("\n")).not.toContain("/c/1.3.0/hooks/hooks.json differs");

    const unreadable = {
      registry: "ok",
      registryError: null,
      installs: [{ installPath: "/c/1.4.0", hooks: null, error: "/c/1.4.0/hooks/hooks.json: ENOENT" }],
    };
    const unread = classifyPluginCache(unreadable, shipped);
    expect(unread.status).toBe("unverifiable");
    expect(classifyPluginCache({ registry: "unreadable", registryError: "bad json", installs: [] }, shipped).status).toBe(
      "unverifiable",
    );
    expect(classifyPluginCache(install(shipped), { nothing: true }).status).toBe("unverifiable");
  });

  it("calls an uninstalled plugin absent, which is neither ok nor drift", () => {
    for (const registry of ["missing", "ok"]) {
      const verdict = classifyPluginCache({ registry, registryError: null, installs: [] }, shipped);
      expect(verdict.status).toBe("absent");
      expect(verdict.violations.join(" ")).toContain("not installed");
    }
  });

  it("reads the installed copies the way the host registers them, and writes nothing", async () => {
    const config = await scratchDir("claude");
    const installPath = path.join(config, "plugins", "cache", "gitpulse", "gitpulse", "1.4.0");
    await mkdir(path.join(installPath, "hooks"), { recursive: true });
    await writeFile(path.join(installPath, "hooks", "hooks.json"), JSON.stringify({ hooks: { Stop: [] } }));
    await writeFile(
      path.join(config, "plugins", "installed_plugins.json"),
      JSON.stringify({ version: 2, plugins: { [PLUGIN_ID]: [{ scope: "user", installPath }], "other@x": [{ installPath: "/nope" }] } }),
    );
    const before = JSON.stringify(
      await Promise.all(
        [path.join(config, "plugins", "installed_plugins.json"), path.join(installPath, "hooks", "hooks.json")].map(async (file) => {
          const info = await stat(file);
          return [file, info.mtimeMs, info.size, await readFile(file, "utf8")];
        }),
      ),
    );
    const read = readInstalledPluginHooks(config);
    expect(read.registry).toBe("ok");
    expect(read.installs).toEqual([{ installPath, hooks: { hooks: { Stop: [] } }, error: null }]);
    const verdict = classifyPluginCache(read, readRepoHooks());
    expect(verdict.status).toBe("drift");
    expect(verdict.violations.join("\n")).toMatch(/missing from the installed cache[^\n]*PreToolUse/);
    const after = JSON.stringify(
      await Promise.all(
        [path.join(config, "plugins", "installed_plugins.json"), path.join(installPath, "hooks", "hooks.json")].map(async (file) => {
          const info = await stat(file);
          return [file, info.mtimeMs, info.size, await readFile(file, "utf8")];
        }),
      ),
    );
    expect(after).toBe(before);
    expect(await readdir(path.join(config, "plugins"))).toEqual(["cache", "installed_plugins.json"]);

    // No registry at all, and a registry that is not JSON.
    const empty = await scratchDir("claude-empty");
    expect(readInstalledPluginHooks(empty)).toEqual({ registry: "missing", registryError: null, installs: [] });
    await mkdir(path.join(empty, "plugins"), { recursive: true });
    await writeFile(path.join(empty, "plugins", "installed_plugins.json"), "{ not json");
    expect(readInstalledPluginHooks(empty).registry).toBe("unreadable");
  });

  it("compares against the hooks this tree really ships, and takes a config directory flag", () => {
    const repo = readRepoHooks() as { hooks: Record<string, unknown> };
    expect(Object.keys(repo.hooks)).toContain("PreToolUse");
    expect(classifyPluginCache(install(repo), repo).status).toBe("ok");
    // parseArgs resolves the flag, which on Windows adds a drive letter and backslashes.
    expect(parseArgs(["--claude-dir", "/tmp/x"]).claudeDir).toBe(path.resolve("/tmp/x"));
    expect(parseArgs([]).claudeDir).toBeUndefined();
  });
});

describe("the lease row", () => {
  const binDir = "/c/lease-bin";
  // classifyLease builds the link with path.join, so the expected value must too.
  const link = path.join(binDir, "cargo");

  it("passes only when a login shell finds the wrapper's own link first", () => {
    expect(classifyLease({ binDir, linkTarget: "/app/gitpulse-hook", servesLease: true, resolved: link, error: null }).status).toBe("gated");
  });

  it("never prints ok for a cargo that shadows the link", () => {
    const verdict = classifyLease({ binDir, linkTarget: "/app/gitpulse-hook", servesLease: true, resolved: "/home/u/.claude-bin/cargo", error: null });
    expect(verdict.status).toBe("shadowed");
    expect(verdict.detail).toContain("/home/u/.claude-bin/cargo");
    const report = formatReport({
      binPath: null, version: null, storeSchema: null, expected: "1", expectedSchema: 1, status: "absent", violations: [],
      lease: { ...verdict, resolved: "/home/u/.claude-bin/cargo", ok: false },
    } as never);
    expect(report).toContain("shadowed by /home/u/.claude-bin/cargo");
    expect(report).toContain("FAIL (shadowed)");
    expect(report).not.toContain("a login shell runs GitPulse's lease wrapper");
  });

  it("says absent when the app made no links, and unverifiable when the shell could not be asked", () => {
    expect(classifyLease({ binDir, linkTarget: null, resolved: null, error: null }).status).toBe("absent");
    expect(classifyLease({ binDir, linkTarget: "/x", servesLease: true, resolved: null, error: "exit 1" }).status).toBe("unverifiable");
    // A link target that could not be asked what it is is not a pass either.
    expect(classifyLease({ binDir, linkTarget: "/x", servesLease: null, resolved: link, error: null }).status).toBe("unverifiable");
  });

  // POSIX-only: runs sh scripts as the login shell and makes a symlink to a shebang script.
  it.skipIf(process.platform === "win32")("asks a real login shell, and sees a profile that puts another cargo first", async () => {
    const root = await scratchDir("lease");
    const bin = path.join(root, "lease-bin");
    const other = path.join(root, "other");
    await mkdir(bin);
    await mkdir(other);
    const hook = path.join(root, "gitpulse-hook");
    await writeFile(hook, "#!/bin/sh\nprintf 'gitpulse-hook 9.9.9\\nsubcommands: collision-guard, lease\\n'\n");
    await chmod(hook, 0o755);
    const { symlink } = await import("node:fs/promises");
    await symlink(hook, path.join(bin, "cargo"));
    const shim = path.join(other, "cargo");
    await writeFile(shim, "#!/bin/sh\n");
    await chmod(shim, 0o755);
    // A shell whose "profile" prepends the other folder, as the owner's does.
    const shell = path.join(root, "fakeshell");
    await writeFile(shell, `#!/bin/sh\nPATH="${other}:$PATH"\nexec /bin/sh -c "$2"\n`);
    await chmod(shell, 0o755);
    const shadowed = probeLease(bin, { shell });
    expect(classifyLease(shadowed).status).toBe("shadowed");
    expect(shadowed.resolved).toBe(shim);
    // And a shell that leaves PATH alone finds the link first.
    const plain = path.join(root, "plainshell");
    await writeFile(plain, '#!/bin/sh\nexec /bin/sh -c "$2"\n');
    await chmod(plain, 0o755);
    expect(classifyLease(probeLease(bin, { shell: plain })).status).toBe("gated");
    // No link: absent, and the shell is never run.
    expect(classifyLease(probeLease(path.join(root, "nowhere"), { shell })).status).toBe("absent");
  });
});

describe("the lease row's link target", () => {
  // POSIX-only: a symlink to a shebang script, run through `sh -lc`.
  it.skipIf(process.platform === "win32")("fails a link to a gitpulse-hook that cannot be the lease wrapper, however the shell resolves cargo", async () => {
    const root = await scratchDir("lease-stale");
    const bin = path.join(root, "lease-bin");
    await mkdir(bin);
    // gitpulse-hook from before build leases: run as cargo, it exits 0 and builds nothing.
    const stale = path.join(root, "gitpulse-hook");
    await writeFile(
      stale,
      "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'gitpulse-hook 0.4.0\\nsubcommands: collision-guard, command-gate\\n'; fi\nexit 0\n",
    );
    await chmod(stale, 0o755);
    const { symlink } = await import("node:fs/promises");
    await symlink(stale, path.join(bin, "cargo"));
    const plain = path.join(root, "plainshell");
    await writeFile(plain, '#!/bin/sh\nexec /bin/sh -c "$2"\n');
    await chmod(plain, 0o755);
    const verdict = classifyLease(probeLease(bin, { shell: plain }));
    expect(verdict.status).toBe("stale");
    expect(verdict.detail).toContain("lease");
    const report = formatReport({
      binPath: null, version: null, storeSchema: null, expected: "1", expectedSchema: 1, status: "absent", violations: [],
      lease: { ...verdict, resolved: path.join(bin, "cargo"), ok: false },
    } as never);
    expect(report).toContain("FAIL (stale)");
    expect(report).not.toContain("a login shell runs GitPulse's lease wrapper");
  });
});
