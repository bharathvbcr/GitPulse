import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_LIB_RS,
  DEFAULT_SRC_DIR,
  ORPHAN_ALLOWLIST,
} from "./check-ipc-contract.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-ipc-contract.mjs", import.meta.url));

const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

async function makeTempDir(prefix: string) {
  const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-ipc-${prefix}-`));
  tempDirs.push(dir);
  return dir;
}

async function runScript(args: string[]) {
  try {
    const { stdout } = await execFileAsync(process.execPath, [scriptPath, ...args], {
      cwd: path.dirname(scriptPath),
    });
    return { code: 0, stdout };
  } catch (err) {
    const failure = err as { code?: number | null; stdout?: string };
    return { code: failure.code ?? 1, stdout: failure.stdout ?? "" };
  }
}

function countIn(stdout: string, label: string): number {
  return Number(stdout.match(new RegExp(`${label}\\s*:\\s*(\\d+)`))?.[1]);
}

describe("check:ipc contract", () => {
  it("passes on the current tree and reports both registry sides", async () => {
    const { code, stdout } = await runScript([]);
    expect(code).toBe(0);

    const count = (label: string) => countIn(stdout, label);
    expect(count("registered handlers")).toBeGreaterThan(0);
    expect(count("invoked commands")).toBeGreaterThan(0);
    expect(count("missing commands")).toBe(0);
    expect(count("orphaned handlers")).toBe(0);
    expect(count("allowed Rust-only")).toBe(Object.keys(ORPHAN_ALLOWLIST).length);
    expect(stdout).toMatch(/OK: IPC contract holds/);
  });

  it("fails when the frontend invokes a command the backend never registered", async () => {
    const dir = await makeTempDir("missing");
    // Deliberately outside src/: proves the checker catches drift wherever a
    // scan root points, without touching tracked frontend sources.
    await writeFile(
      path.join(dir, "scratch.ts"),
      'import { invoke } from "@tauri-apps/api/core";\nawait invoke("cmd_does_not_exist");\n',
    );
    const { code, stdout } = await runScript(["--extra-dir", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/cmd_does_not_exist/);
    expect(stdout).toMatch(/FAIL: IPC contract violated/);
  });

  it("fails when a registered handler has no frontend caller", async () => {
    const dir = await makeTempDir("orphan");
    const libSource = await readFile(DEFAULT_LIB_RS, "utf8");
    const seeded = libSource.replace(
      "            cmd_resolve_git_root,",
      "            cmd_resolve_git_root,\n            cmd_zzz_unwired_handler,",
    );
    expect(seeded).toContain("cmd_zzz_unwired_handler");
    const libCopy = path.join(dir, "lib.rs");
    await writeFile(libCopy, seeded);

    const { code, stdout } = await runScript(["--lib", libCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/orphaned handlers\s*:\s*1/);
    expect(stdout).toMatch(/cmd_zzz_unwired_handler/);
  });

  it("resolves same-file const command names, and fails closed on ones it cannot", async () => {
    const resolvable = await makeTempDir("dynamic-ok");
    await writeFile(
      path.join(resolvable, "dynamic.ts"),
      'import { invoke } from "@tauri-apps/api/core";\nconst CMD_RESOLVE = "cmd_resolve_git_root";\nawait invoke(CMD_RESOLVE);\n',
    );
    const ok = await runScript(["--extra-dir", resolvable]);
    expect(ok.code).toBe(0);
    expect(ok.stdout).not.toMatch(/manual-review sites\s*:\s*[1-9]/);

    const unresolvable = await makeTempDir("dynamic-bad");
    await writeFile(
      path.join(unresolvable, "dynamic.ts"),
      'import { invoke } from "@tauri-apps/api/core";\nconst name = computeName();\nawait invoke(name);\n',
    );
    const bad = await runScript(["--extra-dir", unresolvable]);
    expect(bad.code).toBe(1);
    expect(bad.stdout).toMatch(/manual-review sites\s*:\s*1/);
    expect(bad.stdout).toMatch(/could not be resolved locally/);
  });

  it("scans this repo's real src and src-tauri paths by default", () => {
    expect(DEFAULT_LIB_RS).toMatch(/[\\/]src-tauri[\\/]src[\\/]lib\.rs$/);
    expect(DEFAULT_SRC_DIR).toMatch(/[\\/]src$/);
  });

  it("fails with a clean usage error when a value flag has no value", async () => {
    let stderr = "";
    try {
      await execFileAsync(process.execPath, [scriptPath, "--lib"], {
        cwd: path.dirname(scriptPath),
      });
      throw new Error("expected nonzero exit");
    } catch (err) {
      const failure = err as { code?: number | null; stderr?: string; message: string };
      if (failure.code === undefined) throw err; // our own sentinel
      expect(failure.code).toBe(2);
      stderr = failure.stderr ?? "";
    }
    expect(stderr).toMatch(/--lib requires a value/);
    // Not a raw Node internals dump.
    expect(stderr).not.toMatch(/paths\[0\]|TypeError/);
  });

  it("does not count lookalike invoke callees as invocation sites", async () => {
    const srcDir = await makeTempDir("anchor-src");
    await writeFile(
      path.join(srcDir, "caller.ts"),
      [
        'import { invoke } from "@tauri-apps/api/core";',
        'function safeInvoke(cmd: string) { return cmd; }',
        'safeInvoke("cmd_phantom");',
        "",
      ].join("\n"),
    );
    // Run against the real tree plus one extra dir whose only "call site"
    // goes through a non-invoke alias. Unanchored matching would record
    // `cmd_phantom` as invoked-but-unregistered and blow up the
    // missing-commands ledger.
    const ok = await runScript(["--extra-dir", srcDir]);
    expect(ok.code).toBe(0);
    expect(ok.stdout).not.toMatch(/missing commands\s*:\s*[1-9]/);

    // Control: the same command through the real invoke DOES trip the check.
    await writeFile(
      path.join(srcDir, "caller.ts"),
      'import { invoke } from "@tauri-apps/api/core";\nawait invoke("cmd_phantom");\n',
    );
    const bad = await runScript(["--extra-dir", srcDir]);
    expect(bad.code).toBe(1);
    expect(bad.stdout).toMatch(/missing commands\s*:\s*1/);
  });
});
