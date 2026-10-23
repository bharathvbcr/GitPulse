import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  CHECKED_STRUCTS,
  CONTRACTS,
  applyRenameAll,
  parseRustStructs,
  parseTsInterfaces,
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
  it("passes on the current tree and reports every contract", async () => {
    const { code, stdout } = await runScript([]);
    expect(code).toBe(0);

    expect(countIn(stdout, "contracts checked")).toBe(CONTRACTS.length);
    expect(countIn(stdout, "structs checked")).toBe(
      CONTRACTS.reduce((total, contract) => total + contract.structs.length, 0),
    );
    expect(countIn(stdout, "fields compared")).toBeGreaterThan(0);
    expect(countIn(stdout, "drifted structs")).toBe(0);
    expect(countIn(stdout, "drifted field types")).toBe(0);

    // Every contract is named in the summary: a contract silently dropped
    // from the table would otherwise still report "OK".
    for (const contract of CONTRACTS) {
      expect(stdout, `${contract.label} must appear in the summary`).toContain(contract.label);
    }
    expect(stdout.match(/OK: type contract holds/g)).toHaveLength(1);
    expect(stdout).not.toMatch(/FAIL:/);
  });

  it("covers the coverage and terminal contracts it started with", () => {
    const labels = CONTRACTS.map((contract) => contract.label);
    expect(labels).toContain("coverage");
    expect(labels).toContain("terminal");
    // Widening the table must not quietly drop the two contracts that had
    // already drifted before this checker existed.
    expect(CONTRACTS.find((c) => c.label === "coverage")?.structs).toEqual(CHECKED_STRUCTS);
    expect(CONTRACTS.find((c) => c.label === "terminal")?.structs).toEqual(TERMINAL_STRUCTS);
  });

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

  it("holds a flattened struct's fields to the outer struct's TS twin", async () => {
    const ledger = CONTRACTS.find((c) => c.label === "ledger");
    expect(ledger?.flattenFrom, "LedgerStatus flattens Freshness").toBeTruthy();
    const ok = runTypeCheck(ledger!);
    expect(ok.violations).toEqual([]);

    const freshnessCopy = await scratchCopy(ledger!.flattenFrom!, "flatten-rename");
    const source = await readFile(freshnessCopy, "utf8");
    await writeFile(freshnessCopy, source.replace("pub stale_reason: String,", "pub stale_why: String,"));
    const drifted = runTypeCheck({ ...ledger!, flattenFrom: freshnessCopy });
    expect(drifted.violations.join("\n")).toMatch(
      /LedgerStatus\.stale_why exists in Rust but has no TS property/,
    );
    expect(drifted.violations.join("\n")).toMatch(
      /LedgerStatus\.stale_reason exists in TS but Rust never sends it/,
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

/**
 * The three normalizations that let this checker grow from 2 structs to 31.
 * Each one teaches it that two spellings mean the same wire shape, so each one
 * risks the opposite failure — calling genuinely different shapes equal. These
 * pin both directions.
 */
describe("wire-shape normalization", () => {
  const structOf = (source: string, name: string) =>
    parseRustStructs(source, [name]).structs.get(name)?.fields;

  it("reads a module-qualified type as the same wire type as its bare name", () => {
    const fields = structOf(
      `pub struct P { pub refs: Vec<crate::graph::RefDecoration>, pub one: discovery::Endpoint }`,
      "P",
    );
    expect(fields?.get("refs")?.type).toBe("RefDecoration[]");
    expect(fields?.get("one")?.type).toBe("Endpoint");
  });

  it("does not make two differently named types agree", () => {
    // The danger of stripping paths: only the qualifier is noise, never the
    // name. `a::Foo` and `Bar` must still read as different wire types.
    const fields = structOf(`pub struct P { pub v: crate::a::Foo }`, "P");
    expect(fields?.get("v")?.type).toBe("Foo");
    expect(fields?.get("v")?.type).not.toBe("Bar");
  });

  it("treats an Option omitted by skip_serializing_if as absent-or-T, not null", () => {
    const fields = structOf(
      `pub struct P {
         #[serde(default, skip_serializing_if = "Option::is_none")]
         pub capped: Option<usize>,
         pub plain: Option<usize>
       }`,
      "P",
    );
    // Omitted-when-none is exactly TypeScript's `capped?: number`.
    expect(fields?.get("capped")).toEqual({ type: "number", optional: true });
    // An Option without it really does serialize `null`, and must keep saying so.
    expect(fields?.get("plain")).toEqual({ type: "null|number", optional: false });
  });

  it("resolves rename_all = camelCase the way serde resolves it", () => {
    expect(applyRenameAll("model_info", "camelCase")).toBe("modelInfo");
    expect(applyRenameAll("ready", "camelCase")).toBe("ready");
    // serde capitalizes each underscore-separated segment and joins, so an
    // empty segment collapses; a regex on `_([a-z])` would return `a_B` here.
    expect(applyRenameAll("a__b", "camelCase")).toBe("aB");
    expect(applyRenameAll("model_info", "snake_case")).toBe("model_info");
    expect(applyRenameAll("model_info", undefined)).toBe("model_info");
  });

  it("still refuses a rename_all rule it cannot resolve", () => {
    expect(() => applyRenameAll("model_info", "SCREAMING-KEBAB-CASE")).toThrow(/unsupported/);
    const { violations } = parseRustStructs(
      `#[serde(rename_all = "PascalCase")]
       pub struct P { pub a: u8 }`,
      ["P"],
    );
    expect(violations.join("\n")).toMatch(/PascalCase/);
    // Refused, not guessed at: no struct is reported as checked.
    expect(parseRustStructs(`#[serde(rename_all = "PascalCase")] pub struct P { pub a: u8 }`, ["P"]).structs.size).toBe(0);
  });

  it("lets a per-field rename override rename_all, literally", () => {
    const fields = structOf(
      `#[serde(rename_all = "camelCase")]
       pub struct P {
         pub model_info: u8,
         #[serde(rename = "kept_as_is")]
         pub other_field: u8
       }`,
      "P",
    );
    expect([...(fields?.keys() ?? [])].sort()).toEqual(["kept_as_is", "modelInfo"]);
  });

  /**
   * A string-keyed map is one JSON object whatever map type wrote it. Before
   * this rule no map field could ever agree with its TypeScript twin, so a
   * contract could only cover structs that had none.
   */
  /**
   * A generic wire type is its name with each argument read as a wire type.
   * Before this rule `Reading<u64>` could never agree with TypeScript's
   * `Reading<number>`, so no contract could cover a struct with one.
   */
  it("reads a generic type argument by argument, fixed arrays as homogeneous tuples", () => {
    const rust = structOf(
      `pub struct G {
         pub a: Reading<u64>,
         pub b: crate::sysload::Reading<String>,
         pub c: Reading<[f64; 3]>,
         pub d: Option<Reading<u32>>,
         pub e: Pair<u8, String>
       }`,
      "G",
    );
    expect(rust?.get("a")?.type).toBe("Reading<number>");
    expect(rust?.get("b")?.type).toBe("Reading<string>");
    expect(rust?.get("c")?.type).toBe("Reading<number[]>");
    expect(rust?.get("d")?.type).toBe("Reading<number>|null");
    expect(rust?.get("e")?.type).toBe("Pair<number,string>");
    const ts = parseTsInterfaces(
      `export interface G {
         a: Reading<number>;
         b: Reading<string>;
         c: Reading<[number, number, number]>;
         d: Reading<number> | null;
         e: Pair<number, string>;
       }`,
      ["G"],
    ).interfaces.get("G")?.props;
    for (const field of ["a", "b", "c", "d", "e"]) {
      expect(ts?.get(field)?.type, field).toBe(rust?.get(field)?.type);
    }
    // The arguments still count: a different argument, a different name or
    // a mixed tuple is drift.
    const wrong = parseTsInterfaces(
      `export interface G { a: Reading<string>; b: Other<string>; c: Reading<[number, string]>; }`,
      ["G"],
    ).interfaces.get("G")?.props;
    expect(wrong?.get("a")?.type).not.toBe(rust?.get("a")?.type);
    expect(wrong?.get("b")?.type).not.toBe(rust?.get("b")?.type);
    expect(wrong?.get("c")?.type).not.toBe(rust?.get("c")?.type);
  });

  it("reads a string-keyed map as the Record its TypeScript twin declares, value type included", () => {
    const rust = structOf(
      `pub struct V {
         pub a: std::collections::BTreeMap<String, Vec<String>>,
         pub b: HashMap<String, u32>,
         pub c: BTreeMap<String, crate::tool_config::ModelChoice>,
         pub d: BTreeMap<u32, String>
       }`,
      "V",
    );
    expect(rust?.get("a")?.type).toBe("Record<string,string[]>");
    expect(rust?.get("b")?.type).toBe("Record<string,number>");
    expect(rust?.get("c")?.type).toBe("Record<string,ModelChoice>");
    // A non-string key is not claimed to be a string-keyed record.
    expect(rust?.get("d")?.type).not.toMatch(/^Record</);
    const ts = parseTsInterfaces(
      `export interface V {
         a: Record<string, string[]>;
         b: { [key: string]: number };
         c: Record<string, ModelChoice>;
       }`,
      ["V"],
    ).interfaces.get("V")?.props;
    expect(ts?.get("a")?.type).toBe(rust?.get("a")?.type);
    expect(ts?.get("b")?.type).toBe(rust?.get("b")?.type);
    expect(ts?.get("c")?.type).toBe(rust?.get("c")?.type);
    // The value type still counts: a map of the wrong values is drift.
    const wrong = parseTsInterfaces(`export interface V { a: Record<string, number[]>; }`, ["V"]).interfaces.get("V")?.props;
    expect(wrong?.get("a")?.type).not.toBe(rust?.get("a")?.type);
  });
});

// A flattened struct's TS mirror: every mutation of it must be caught.
const ledgerContract = CONTRACTS.find((c) => c.label === "ledger")!;
const freshnessContract = CONTRACTS.find((c) => c.label === "ledger-freshness")!;

async function mutatedTs(edit: (source: string) => string) {
  const dir = await makeTempDir("flatten-mutation");
  const copy = path.join(dir, "types.ts");
  const source = await readFile(ledgerContract.tsPath, "utf8");
  const next = edit(source);
  expect(next, "the mutation must change the TS mirror").not.toBe(source);
  await writeFile(copy, next);
  return copy;
}

const mutations: Array<[string, (s: string) => string]> = [
  ["drop Freshness.stale_reason", (s) => s.replace("  stale_reason: string;\n", "")],
  ["drop Freshness.freshness_ok", (s) => s.replace("  freshness_ok: boolean;\n", "")],
  ["rename Freshness.stale_after_secs", (s) => s.replace("  stale_after_secs: number;", "  staleAfterSecs: number;")],
  ["retype Freshness.stale to string", (s) => s.replace("  stale: boolean;", "  stale: string;")],
  ["retype Freshness.catch_up (drop null)", (s) => s.replace("  catch_up: CatchUpSummary | null;", "  catch_up: CatchUpSummary;")],
  ["drop LedgerStatus.history_only_bindings", (s) => s.replace("  history_only_bindings: number;\n", "")],
  ["drop `extends Freshness` from LedgerStatus", (s) => s.replace("export interface LedgerStatus extends Freshness {", "export interface LedgerStatus {")],
  ["retype LedgerStatus.recording_state to number", (s) => s.replace("  recording_state: string;", "  recording_state: number;")],
  ["drop CatchUpSummary.stale_worktrees", (s) => s.replace("  stale_worktrees: number;\n", "")],
  ["retype CatchUpSummary.recorded", (s) => s.replace("  recorded: number;", "  recorded: string;")],
];

describe("check:types on a flattened LedgerStatus", () => {
  it("passes on the unmutated tree (control)", () => {
    expect(runTypeCheck(ledgerContract).violations).toEqual([]);
    expect(runTypeCheck(freshnessContract).violations).toEqual([]);
  });

  for (const [name, edit] of mutations) {
    it(`fails the ledger contract or the freshness contract when: ${name}`, async () => {
      const tsPath = await mutatedTs(edit);
      const a = runTypeCheck({ ...ledgerContract, tsPath });
      const b = runTypeCheck({ ...freshnessContract, tsPath });
      expect(
        [...a.violations, ...b.violations],
        `check:types accepted the mutation "${name}" in both contracts`,
      ).not.toEqual([]);
    });

    // Stronger: the contract that OWNS the flatten (LedgerStatus) should itself
    // notice a Freshness-field mutation, not rely on a second contract.
    if (name.includes("Freshness.")) {
      it(`the ledger contract alone fails when: ${name}`, async () => {
        const tsPath = await mutatedTs(edit);
        expect(
          runTypeCheck({ ...ledgerContract, tsPath }).violations,
          `the "ledger" contract accepted "${name}"`,
        ).not.toEqual([]);
      });
    }
  }
});
