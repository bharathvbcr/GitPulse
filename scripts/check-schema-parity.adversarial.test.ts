// Adversarial coverage for the EXTRACTION_SCHEMA_VERSION parity row
// (ft-576fb6dc). The cases marked BREAK were `it.fails` until they were fixed.
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { SCHEMAS, devmapReading, judge, rustConstant, schemasAt, summarize } from "./check-schema-parity.mjs";
import { readCliSchema } from "./check-vendor-schema.mjs";
import { wallClockBudget } from "../src/lib/testing/wallClock";

const C = "EXTRACTION_SCHEMA_VERSION";
const owner = { store: 27, dcStore: 9, workbench: 13, extract: 94 };

describe("rustConstant, extraction constant lookalikes", () => {
  it("reads CRLF files, &'static str and a leading-zero string", () => {
    expect(rustConstant(`pub const ${C}: &str = "6";\r\n`, C)).toBe(6);
    expect(rustConstant(`pub const ${C}: &'static str = "7";`, C)).toBe(7);
    expect(rustConstant(`pub const ${C}: &str = "094";`, C)).toBe(94);
  });

  it("refuses values beyond 2^53, hex, underscores and non-literal forms", () => {
    expect(rustConstant(`pub const ${C}: &str = "9007199254740993";`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u64 = 9007199254740993;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 0x10;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 1_000;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: &str = concat!("9");`, C)).toBeNull();
  });

  it("does not match a constant whose name merely contains the one asked for", () => {
    expect(rustConstant("pub const CURRENT_SCHEMA_VERSION: i32 = 27;", "SCHEMA_VERSION")).toBeNull();
    expect(rustConstant("pub const MIN_READER_SCHEMA_VERSION: i32 = 25;", "SCHEMA_VERSION")).toBeNull();
    expect(rustConstant(`pub const ${C}_X: u32 = 8;`, C)).toBeNull();
  });

  it("treats a cfg-gated pair with different values as unreadable, not as the first one", () => {
    expect(rustConstant(`#[cfg(a)]\npub const ${C}: &str = "4";\n#[cfg(not(a))]\npub const ${C}: &str = "5";`, C)).toBeNull();
  });

  it("is loud, not wrong, for pub(crate)", () => {
    expect(rustConstant(`pub(crate) const ${C}: &str = "3";`, C)).toBeNull();
  });

  // BREAK: a declaration inside a comment is read as the real constant, so a
  // file whose live constant was deleted or renamed but whose doc comment
  // still quotes the old line reports a version that nothing compiles.
  it("ignores a lookalike declaration in a line, doc or block comment", () => {
    expect(rustConstant(`// pub const ${C}: &str = "1";\n`, C)).toBeNull();
    expect(rustConstant(`/// pub const ${C}: &str = "1";\n`, C)).toBeNull();
    expect(rustConstant(`/* pub const ${C}: &str = "1"; */`, C)).toBeNull();
  });

  // BREAK (same root): a commented-out old value next to the live one turns a
  // readable constant into null, which readSchemas then throws on, so the
  // whole check stops with "owner unreadable" rather than reading the live one.
  it("reads the live constant when a comment above it quotes an old value", () => {
    expect(rustConstant(`// pub const ${C}: &str = "1";\npub const ${C}: &str = "2";`, C)).toBe(2);
  });
});

describe("rustConstant against the real trees", () => {
  const roots = [
    { name: "DevCouncil", root: "/Users/bharath/Code/devtools/DevCouncil/rust" },
    { name: "vendored", root: path.join(import.meta.dirname, "..", "src-tauri", "vendored") },
  ];
  for (const { name, root } of roots) {
    it.skipIf(!existsSync(root))(`${name}: all four constants are whole numbers`, () => {
      for (const { constant, file } of Object.values(SCHEMAS) as { constant: string; file: string }[]) {
        const value = rustConstant(readFileSync(path.join(root, file), "utf8"), constant);
        expect(value, `${constant} in ${file}`).toBeGreaterThan(0);
      }
    });
  }
});

describe("judge and summarize", () => {
  it("a reader that lists extract but reports none is never a match", () => {
    expect(judge(owner, { reader: "r", keys: ["store", "extract"], schemas: { store: 27 } }).verdict).toBe("unverifiable");
    expect(judge(owner, { reader: "r", keys: ["extract"], schemas: {} }).verdict).toBe("unverifiable");
  });

  it("absent still wins, and a known mismatch outranks an unreported key", () => {
    expect(judge(owner, { reader: "r", keys: ["store", "extract"], absent: "not on PATH" }).verdict).toBe("absent");
    expect(judge(owner, { reader: "r", keys: ["store", "extract"], schemas: { store: 27, extract: 87 } }).verdict).toBe("stale");
    expect(judge(owner, { reader: "r", keys: ["store", "extract"], schemas: { store: 26 } }).verdict).toBe("stale");
  });

  it("5 match + 3 unverifiable is FAIL", () => {
    const rows = [...Array(5).fill({ verdict: "match" }), ...Array(3).fill({ verdict: "unverifiable" })];
    expect(summarize(rows)).toEqual({ ok: false, summary: "FAIL: 5 match · 3 unverifiable" });
  });

  // BREAK (low): NaN is typeof "number", so a NaN reading is judged stale
  // ("NaN, DevCouncil writes 94") instead of unverifiable. Every shipped
  // reader validates upstream, so this is unreachable today.
  it("a NaN reading is unverifiable, not stale", () => {
    expect(judge(owner, { reader: "r", keys: ["extract"], schemas: { extract: Number.NaN } }).verdict).toBe("unverifiable");
  });
});

describe("schemasAt on a commit that predates the constant", () => {
  it("returns the constants it can read and leaves the rest out, without throwing", () => {
    const devcouncil = "/Users/bharath/Code/devtools/DevCouncil";
    if (!existsSync(devcouncil)) return;
    const failed = { status: 128, stdout: "", stderr: "fatal: path does not exist", error: undefined };
    // The double returns only the fields schemasAt reads, not a full spawnSync
    // result, so it is cast through unknown to the injected runner's type.
    const gate = ((_cmd: string, args: readonly string[]) =>
      args[0] === "cat-file" ? { status: 0, stdout: "", error: undefined } : failed) as unknown as typeof spawnSync;
    expect(schemasAt(devcouncil, "a".repeat(40), gate)).toEqual({});
  });
});

describe("devmap (PATH) extract reading", () => {
  it("a devmap whose --version omits extract is unverifiable, never a match", () => {
    const row = judge(owner, devmapReading({ available: true, expected: 27, extract: null, versionLine: "x", source: "doctor" }));
    expect(row.verdict).toBe("unverifiable");
  });

  it("a devmap that prints extract 87 is stale", () => {
    const row = judge(owner, devmapReading({ available: true, expected: 27, extract: 87, versionLine: "x", source: "doctor" }));
    expect(row.verdict).toBe("stale");
  });
});

describe.skipIf(process.platform === "win32")("readCliSchema with a misbehaving devmap", () => {
  const dirs: string[] = [];
  const realPath = process.env.PATH;
  afterEach(async () => {
    process.env.PATH = realPath;
    await Promise.all(dirs.splice(0).map((dir) => rm(dir, { recursive: true, force: true })));
  });

  async function withDevmap(versionBody: string) {
    const dir = await mkdtemp(path.join(tmpdir(), "adv-devmap-"));
    dirs.push(dir);
    const bin = path.join(dir, "devmap");
    await writeFile(bin, `#!/bin/sh
if [ "$1" = "doctor" ]; then echo '{"expected_schema_version":27,"code_graph_schema_version":2,"version":"0.2.4"}'; exit 0; fi
if [ "$1" = "--version" ]; then ${versionBody}; fi
`);
    await chmod(bin, 0o755);
    process.env.PATH = `${dir}:${realPath}`;
  }

  it("a --version that only writes stderr leaves extract null and the schema intact", async () => {
    await withDevmap(`echo "devmap 0.2.4 (store schema 27, extract schema 94)" >&2; exit 0`);
    expect(readCliSchema()).toMatchObject({ available: true, expected: 27, extract: null });
  });

  it("a --version that exits non-zero leaves extract null", async () => {
    await withDevmap(`echo "extract schema 94"; exit 3`);
    expect(readCliSchema()).toMatchObject({ available: true, expected: 27, extract: null });
  });

  it("a --version that hangs is cut off by the 15s timeout and does not fail doctor's answer", async () => {
    await withDevmap(`sleep 60`);
    const started = Date.now();
    expect(readCliSchema()).toMatchObject({ available: true, expected: 27, extract: null });
    expect(Date.now() - started).toBeLessThan(wallClockBudget(20_000));
  }, 40_000);

  it("a --version that prints extract schema 94 is read", async () => {
    await withDevmap(`echo "devmap 0.2.4 (store schema 27, extract schema 94)"; exit 0`);
    expect(readCliSchema()).toMatchObject({ available: true, expected: 27, extract: 94 });
  });
});
