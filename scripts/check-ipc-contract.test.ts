import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_LIB_RS,
  DEFAULT_SRC_DIR,
  ORPHAN_ALLOWLIST,
  formatReport,
  runContractCheck,
  scanAnnotatedCommands,
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

  it("recognizes invoke calls whose generic argument nests one level deep", async () => {
    // Regression: `invoke<Guarded<string>>("cmd_…")` used to be invisible to
    // the scanner (the old regex could not span the nested `>`), so real call
    // sites silently vanished and their handlers read as orphaned.
    const nested = await makeTempDir("generic-nested");
    await writeFile(
      path.join(nested, "nested.ts"),
      'import { invoke } from "@tauri-apps/api/core";\n'
        + 'interface Guarded<T> { output: T }\n'
        + 'const r = invoke<Guarded<string>>("cmd_github_cancel_run", { runId: 1 });\n'
        + 'const s = await invoke<Guarded<Guarded<string>>>("cmd_github_rerun_run", { runId: 2 });\n'
        + 'const t = await invoke<Guarded<Guarded<string[]>>>("cmd_github_checkout_pr", { number: 3 });\n'
        + 'console.log(r, s, t);\n',
    );
    const ok = await runScript(["--extra-dir", nested]);
    expect(ok.code).toBe(0);
    expect(ok.stdout).not.toMatch(/orphaned handlers\s*:\s*[1-9]/);
    expect(ok.stdout).not.toMatch(/manual-review sites\s*:\s*[1-9]/);

    // One level of nesting is fully scanned: an unregistered command behind
    // a nested-generic invoke still fails loudly as a missing command.
    const tooDeep = await makeTempDir("generic-too-deep");
    await writeFile(
      path.join(tooDeep, "deep.ts"),
      'import { invoke } from "@tauri-apps/api/core";\n'
        + 'type D<A> = A;\n'
        + 'await invoke<D<string>>("cmd_definitely_not_registered_xyz");\n',
    );
    const deep = await runScript(["--extra-dir", tooDeep]);
    expect(deep.code).toBe(1);
    expect(deep.stdout).toMatch(/missing commands\s*:\s*1/);
    expect(deep.stdout).toMatch(/cmd_definitely_not_registered_xyz/);

    // Arbitrarily deep nesting must stay visible too: depth-counted bracket
    // matching, not a fixed nesting budget. An unregistered name behind three
    // levels still fails loudly.
    const triple = await makeTempDir("generic-triple");
    await writeFile(
      path.join(triple, "triple.ts"),
      'import { invoke } from "@tauri-apps/api/core";\n'
        + 'interface Guarded<T> { output: T }\n'
        + 'const t = invoke<Guarded<Guarded<string[]>>>("cmd_definitely_not_registered_triple");\n'
        + 'console.log(t);\n',
    );
    const tripleRun = await runScript(["--extra-dir", triple]);
    expect(tripleRun.code).toBe(1);
    expect(tripleRun.stdout).toMatch(/missing commands\s*:\s*1/);
    expect(tripleRun.stdout).toMatch(/cmd_definitely_not_registered_triple/);

    // Generics may span lines (object-typed payloads read better that way):
    // an unregistered command behind a multi-line generic still fails loudly.
    const multiline = await makeTempDir("generic-multiline");
    await writeFile(
      path.join(multiline, "multiline.ts"),
      'import { invoke } from "@tauri-apps/api/core";\n'
        + 'const res = await invoke<{\n'
        + '  stdout_tail: string;\n'
        + '  exit_code: number | null;\n'
        + '}>("cmd_definitely_not_registered_multiline", {});\n'
        + 'console.log(res);\n',
    );
    const multilineRun = await runScript(["--extra-dir", multiline]);
    expect(multilineRun.code).toBe(1);
    expect(multilineRun.stdout).toMatch(/missing commands\s*:\s*1/);
    expect(multilineRun.stdout).toMatch(/cmd_definitely_not_registered_multiline/);

    // String contents are opaque: a `<` inside a literal must not open a
    // span that blanks later call sites.
    const quoted = await makeTempDir("generic-quoted");
    await writeFile(
      path.join(quoted, "quoted.ts"),
      'import { invoke } from "@tauri-apps/api/core";\n'
        + 'const hint = "use x<y then z>w";\n'
        + 'await invoke("cmd_resolve_git_root", {});\n'
        + 'await invoke("cmd_definitely_not_registered_quoted", {});\n'
        + 'console.log(hint);\n',
    );
    const quotedRun = await runScript(["--extra-dir", quoted]);
    expect(quotedRun.code).toBe(1);
    // Only the genuinely unregistered command fails; the registered one
    // beside it proves the string's stray brackets hid nothing.
    expect(quotedRun.stdout).toMatch(/missing commands\s*:\s*1/);
    expect(quotedRun.stdout).toMatch(/cmd_definitely_not_registered_quoted/);
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

describe("report alignment (A3)", () => {
  it("keeps the metric column aligned regardless of how long the lib label is", () => {
    const result = {
      registeredCount: 95,
      invokedCount: 87,
      siteCount: 106,
      productionFiles: 157,
      orphans: [] as string[],
      missing: [] as string[],
      allowedOrphans: [] as string[],
      staleAllowlist: [] as string[],
      manualReviews: [] as { file: string; line: number; detail: string }[],
      violations: [] as string[],
      unregistered: [] as string[],
      annotatedCount: 95,
      ok: true,
    };
    const long = formatReport(result, "a/very/deeply/nested/path/to/src-tauri/src/lib.rs".repeat(3));
    const metricLines = long.split("\n").filter((line) => /^ {2}\S/.test(line) && line.includes(": "));
    const columns = metricLines.map((line) => line.indexOf(": "));
    expect(metricLines.length).toBeGreaterThan(3);
    expect(new Set(columns).size).toBe(1);
  });
});

describe("annotated but unregistered commands", () => {
  it("finds every #[tauri::command] in the crate, including async and attribute-separated ones", () => {
    const found = scanAnnotatedCommands([
      fileURLToPath(new URL("../src-tauri/src", import.meta.url)),
    ]);
    // Cross-checked three ways against the real crate: the generate_handler!
    // list, a raw attribute count, and this scanner all report the same total.
    expect(found.size).toBe(121);
    expect(found.has("cmd_stage_file")).toBe(true);
    for (const [, site] of found) {
      expect(site.file).toMatch(/^src-tauri\/src\//);
      expect(site.line).toBeGreaterThan(0);
    }
  });

  it("reports a command that is annotated but absent from generate_handler!", async () => {
    // This compiles cleanly and was invisible to every gate: the registry does
    // not list it so it is not an orphan, and no frontend calls it so it is
    // not missing. It is a dead IPC entry point.
    const dir = await mkdtemp(path.join(tmpdir(), "gp-unregistered-"));
    tempDirs.push(dir);
    const srcDir = path.join(dir, "src-tauri", "src");
    await mkdir(srcDir, { recursive: true });
    await writeFile(
      path.join(srcDir, "lib.rs"),
      "fn main() {\n  tauri::generate_handler![commands::cmd_live];\n}\n",
    );
    await writeFile(
      path.join(srcDir, "commands.rs"),
      [
        "#[tauri::command]",
        "pub fn cmd_live() -> String { String::new() }",
        "",
        "#[tauri::command(async)]",
        "/// a doc comment between the attribute and the fn",
        "pub async fn cmd_dead(repo: String) -> Result<(), String> { let _ = repo; Ok(()) }",
      ].join("\n"),
    );
    const found = scanAnnotatedCommands([srcDir]);
    expect([...found.keys()].sort()).toEqual(["cmd_dead", "cmd_live"]);

    const frontend = path.join(dir, "src");
    await mkdir(frontend, { recursive: true });
    await writeFile(path.join(frontend, "app.ts"), 'invoke("cmd_live");\n');
    const result = runContractCheck({
      libPath: path.join(srcDir, "lib.rs"),
      srcDirs: [frontend],
    });
    expect(result.ok).toBe(false);
    expect(result.unregistered).toEqual(["cmd_dead"]);
    expect(result.violations.join(" ")).toContain("cmd_dead");
    // The text report must name it, not just the JSON.
    expect(formatReport(result, "lib.rs")).toContain("annotated but never registered");
  });
});
