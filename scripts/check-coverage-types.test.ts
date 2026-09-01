import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  CHECKED_STRUCTS,
  DEFAULT_RUST_SOURCE,
  DEFAULT_TS_SOURCE,
  TERMINAL_RUST_SOURCE,
  TERMINAL_STRUCTS,
  TERMINAL_TS_SOURCE,
  runTypeCheck,
} from "./check-coverage-types.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-coverage-types.mjs", import.meta.url));

const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

async function makeTempDir(prefix: string) {
  const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-covtypes-${prefix}-`));
  tempDirs.push(dir);
  return dir;
}

/** Copy a tracked source into a scratch dir so drift never touches the tree. */
async function scratchCopy(sourcePath: string, prefix: string) {
  const dir = await makeTempDir(prefix);
  const copy = path.join(dir, path.basename(sourcePath));
  await writeFile(copy, await readFile(sourcePath, "utf8"));
  return copy;
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

describe("check:types coverage contract", () => {
  it("passes on the current tree and reports both sides", async () => {
    const { code, stdout } = await runScript([]);
    expect(code).toBe(0);

    // With no flags every contract runs, so counts are asserted per report.
    const [coverage, terminal] = stdout.split(/(?=^Terminal IPC type check)/m);
    expect(countIn(coverage, "rust structs checked")).toBe(CHECKED_STRUCTS.length);
    expect(countIn(coverage, "ts interfaces checked")).toBe(CHECKED_STRUCTS.length);
    expect(countIn(coverage, "drifted structs")).toBe(0);
    expect(countIn(coverage, "fields compared")).toBeGreaterThan(0);

    expect(terminal, "the terminal contract must be checked too").toBeDefined();
    expect(countIn(terminal, "rust structs checked")).toBe(TERMINAL_STRUCTS.length);
    expect(countIn(terminal, "ts interfaces checked")).toBe(TERMINAL_STRUCTS.length);
    expect(countIn(terminal, "drifted structs")).toBe(0);
    expect(countIn(terminal, "fields compared")).toBeGreaterThan(0);

    expect(stdout.match(/OK: type contract holds/g)).toHaveLength(2);
    expect(stdout).not.toMatch(/FAIL:/);
  });

  /**
   * `TerminalRunResult` is the payload every command-running panel reads and
   * it had no gate at all — it was declared three times in TypeScript, so a
   * backend rename would land as a silently `undefined` property in whichever
   * panel had not been updated.
   */
  it("fails when the terminal wire type drifts from its Rust struct", async () => {
    const tsCopy = await scratchCopy(TERMINAL_TS_SOURCE, "terminal-drift");
    const source = await readFile(tsCopy, "utf8");
    await writeFile(tsCopy, source.replace("stdout_tail: string;", "stdoutTail: string;"), "utf8");

    const result = runTypeCheck({
      rustPath: TERMINAL_RUST_SOURCE,
      tsPath: tsCopy,
      structs: TERMINAL_STRUCTS,
    });
    expect(result.ok).toBe(false);
    expect(result.violations.join("\n")).toMatch(
      /TerminalRunResult\.stdout_tail exists in Rust but has no TS property/,
    );
    expect(result.violations.join("\n")).toMatch(
      /TerminalRunResult\.stdoutTail exists in TS but Rust never sends it/,
    );
  });

  it("fails when a Rust field is renamed away from its TS twin", async () => {
    const rustCopy = await scratchCopy(DEFAULT_RUST_SOURCE, "rust-rename");
    const source = await readFile(rustCopy, "utf8");
    // First occurrence lives in CoverageTotals; replace-first keeps the edit
    // surgical without touching tracked sources.
    const seeded = source.replace("pub lines_hit: usize,", "pub lines_struck: usize,");
    expect(seeded).toContain("lines_struck");
    await writeFile(rustCopy, seeded);

    const { code, stdout } = await runScript(["--rust", rustCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/drift: CoverageTotals\.lines_struck exists in Rust but has no TS property/);
    expect(stdout).toMatch(/drift: CoverageTotals\.lines_hit exists in TS but Rust never sends it/);
    expect(stdout).toMatch(/FAIL: type contract violated/);
  });

  it("fails when a shared Rust field changes wire type without changing its name", async () => {
    const rustCopy = await scratchCopy(DEFAULT_RUST_SOURCE, "rust-type-drift");
    const source = await readFile(rustCopy, "utf8");
    const seeded = source.replace("pub lines_hit: usize,", "pub lines_hit: String,");
    expect(seeded).toContain("pub lines_hit: String");
    await writeFile(rustCopy, seeded);

    const { code, stdout } = await runScript(["--rust", rustCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(
      /drift: CoverageTotals\.lines_hit type mismatch \(Rust string; TS number\)/,
    );
  });

  it("fails when a shared TypeScript field changes wire type without changing its name", async () => {
    const tsCopy = await scratchCopy(DEFAULT_TS_SOURCE, "ts-type-drift");
    const source = await readFile(tsCopy, "utf8");
    const seeded = source.replace("  lines_hit: number;", "  lines_hit: string;");
    expect(seeded).toContain("lines_hit: string");
    await writeFile(tsCopy, seeded);

    const { code, stdout } = await runScript(["--ts", tsCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(
      /drift: CoverageTotals\.lines_hit type mismatch \(Rust number; TS string\)/,
    );
  });

  it("honors per-field serde renames when matching wire names", async () => {
    const rustCopy = await scratchCopy(DEFAULT_RUST_SOURCE, "serde-rename");
    const source = await readFile(rustCopy, "utf8");
    // CoveredLine.hits gains a wire rename: the TS side must use the wire
    // name, not the Rust ident.
    const seeded = source.replace(
      "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\npub struct CoveredLine {",
      "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\npub struct CoveredLine {\n    #[serde(rename = \"hit_count\")]",
    );
    expect(seeded).toMatch(/rename = "hit_count"/);
    await writeFile(rustCopy, seeded);

    const { code, stdout } = await runScript(["--rust", rustCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/drift: CoveredLine\.hit_count exists in Rust but has no TS property/);
    expect(stdout).not.toMatch(/CoveredLine\.hits exists in Rust/);
  });

  it("fails when TS grows a property Rust never sends", async () => {
    const tsCopy = await scratchCopy(DEFAULT_TS_SOURCE, "ts-extra");
    const source = await readFile(tsCopy, "utf8");
    const seeded = source.replace(
      "export interface CoverageReport {",
      "export interface CoverageReport {\n  phantom_field: number;",
    );
    expect(seeded).toContain("phantom_field");
    await writeFile(tsCopy, seeded);

    const { code, stdout } = await runScript(["--ts", tsCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(
      /drift: CoverageReport\.phantom_field exists in TS but Rust never sends it/,
    );
  });

  it("fails when TS loses a property Rust still sends", async () => {
    const tsCopy = await scratchCopy(DEFAULT_TS_SOURCE, "ts-missing");
    const source = await readFile(tsCopy, "utf8");
    // Anchor inside CoverageReport so the removal targets that interface even
    // if identically-named properties exist elsewhere in the file.
    const marker = "export interface CoverageReport {";
    const idx = source.indexOf(marker);
    expect(idx).toBeGreaterThan(-1);
    const tail = source.slice(idx).replace("  truncated: boolean;\n", "");
    expect(tail).not.toMatch(/^\s+truncated:/m);
    await writeFile(tsCopy, source.slice(0, idx) + tail);

    const { code, stdout } = await runScript(["--ts", tsCopy]);
    expect(code).toBe(1);
    expect(stdout).toMatch(
      /drift: CoverageReport\.truncated exists in Rust but has no TS property/,
    );
  });

  it("defaults point at this repo's real coverage type sources", () => {
    expect(DEFAULT_RUST_SOURCE).toMatch(/[\\/]src-tauri[\\/]src[\\/]analyzer[\\/]coverage\.rs$/);
    expect(DEFAULT_TS_SOURCE).toMatch(/[\\/]src[\\/]lib[\\/]coverage[\\/]types\.ts$/);
  });
});
