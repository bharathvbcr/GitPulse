import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import {
  SCHEMAS,
  SERVE_CONTRACT,
  contractRow,
  devmapReading,
  jarvisPin,
  judge,
  manviPin,
  parseArgs,
  pinReading,
  readSchemas,
  rustConstant,
  schemasAt,
  summarize,
} from "./check-schema-parity.mjs";

const owner = { store: 27, dcStore: 9, workbench: 13, extract: 94 };
const ALL = ["store", "dcStore", "workbench", "extract"] as const;

const tempDirs: string[] = [];
afterAll(() => {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
});

function git(cwd: string, ...args: string[]): string {
  const out = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (out.status !== 0) throw new Error(`git ${args.join(" ")}: ${out.stderr}`);
  return out.stdout.trim();
}

/** Writes DevCouncil's three schema files at these numbers under `rust/`. */
function writeOwner(root: string, schemas: { store: number; dcStore: number; workbench: number; extract: number }) {
  const files: Record<string, string> = {
    [SCHEMAS.store.file]: `pub const CURRENT_SCHEMA_VERSION: i32 = ${schemas.store};\npub const MIN_READER_SCHEMA_VERSION: i32 = 25;\n`,
    [SCHEMAS.dcStore.file]: `pub const SCHEMA_VERSION: u32 = ${schemas.dcStore};\n`,
    [SCHEMAS.workbench.file]: `pub const WORKBENCH_SCHEMA: i64 = ${schemas.workbench};\n`,
    [SCHEMAS.extract.file]: `pub const EXTRACTION_SCHEMA_VERSION: &str = "${schemas.extract}";\n`,
  };
  for (const [file, text] of Object.entries(files)) {
    const full = path.join(root, "rust", file);
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, text);
  }
}

/** A DevCouncil-shaped repository with three commits; returns their shas. */
function devcouncilFixture() {
  const root = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-parity-"));
  tempDirs.push(root);
  git(root, "init", "-q");
  git(root, "config", "user.email", "fixture@example.invalid");
  git(root, "config", "user.name", "fixture");
  git(root, "config", "commit.gpgsign", "false");
  writeOwner(root, { ...owner, store: 26 });
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "store 26");
  const old = git(root, "rev-parse", "HEAD");
  writeOwner(root, owner);
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "store 27");
  const bump = git(root, "rev-parse", "HEAD");
  writeFileSync(path.join(root, "README.md"), "unrelated\n");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "docs only");
  const head = git(root, "rev-parse", "HEAD");
  return { root, old, bump, head };
}

describe("rustConstant", () => {
  it("reads the integer constant whatever its integer type", () => {
    expect(rustConstant("pub const CURRENT_SCHEMA_VERSION: i32 = 27;", "CURRENT_SCHEMA_VERSION")).toBe(27);
    expect(rustConstant("pub const SCHEMA_VERSION: u32 = 9;", "SCHEMA_VERSION")).toBe(9);
    expect(rustConstant("pub const WORKBENCH_SCHEMA: i64 = 13;", "WORKBENCH_SCHEMA")).toBe(13);
  });

  it("does not read one constant as another whose name it ends with", () => {
    expect(rustConstant("pub const MIN_READER_SCHEMA_VERSION: i32 = 25;", "SCHEMA_VERSION")).toBeNull();
    expect(rustConstant("pub const DC_SCHEMA_VERSION: u32 = 4;", "SCHEMA_VERSION")).toBeNull();
  });

  it("reads devmap-extract's string constant as the number it names", () => {
    expect(rustConstant('pub const EXTRACTION_SCHEMA_VERSION: &str = "94";', "EXTRACTION_SCHEMA_VERSION")).toBe(94);
    expect(rustConstant("pub const EXTRACTION_SCHEMA_VERSION: &'static str = \"7\" ;", "EXTRACTION_SCHEMA_VERSION")).toBe(7);
  });

  it("does not find a string constant that is not a whole number", () => {
    for (const bad of ['"94a"', '""', '"-1"', '"9.4"', '" 94"', '"0x5e"']) {
      expect(rustConstant(`pub const EXTRACTION_SCHEMA_VERSION: &str = ${bad};`, "EXTRACTION_SCHEMA_VERSION")).toBeNull();
    }
  });

  it("counts only live declarations: comments are blanked, string contents are not", () => {
    const C = "EXTRACTION_SCHEMA_VERSION";
    expect(rustConstant(`/* old\npub const ${C}: &str = "1";\nstill comment */\npub const ${C}: &str = "2";`, C)).toBe(2);
    expect(rustConstant(`/* a /* nested */ pub const ${C}: &str = "1"; */\npub const ${C}: &str = "3";`, C)).toBe(3);
    expect(rustConstant(`pub const ${C}: &str = "4"; // pub const ${C}: &str = "9";`, C)).toBe(4);
    expect(rustConstant(`const URL: &str = "http://x/*";\npub const ${C}: &str = "5";\n// */`, C)).toBe(5);
    expect(rustConstant(`const Q: char = '"';\npub const ${C}: &str = "6";`, C)).toBe(6);
  });

  it("refuses an owner whose extract constant is not a number rather than leaving it undefined", () => {
    const read = (file: string) =>
      file === SCHEMAS.extract.file
        ? 'pub const EXTRACTION_SCHEMA_VERSION: &str = "94a";'
        : "pub const CURRENT_SCHEMA_VERSION: i32 = 1;\npub const SCHEMA_VERSION: u32 = 1;\npub const WORKBENCH_SCHEMA: i64 = 1;";
    expect(() => readSchemas(read)).toThrow(/EXTRACTION_SCHEMA_VERSION not found/);
  });

  it("refuses an owner file without the constant rather than leaving it undefined", () => {
    expect(() => readSchemas(() => "// nothing here")).toThrow(/CURRENT_SCHEMA_VERSION not found/);
  });
});

describe("judge", () => {
  it("matches a reader that reports what the owner writes", () => {
    expect(judge(owner, { reader: "r", keys: [...ALL], schemas: { ...owner } }).verdict).toBe("match");
  });

  it("calls a reader one schema behind stale and names both numbers", () => {
    const row = judge(owner, { reader: "gitpulse-mcp", keys: [...ALL], schemas: { ...owner, store: 26 } });
    expect(row.verdict).toBe("stale");
    expect(row.detail).toContain("devmap store 26, DevCouncil writes 27");
  });

  it("calls a reader ahead of the owner stale too: it is not reading what is written", () => {
    expect(judge(owner, { reader: "r", keys: ["workbench"], schemas: { workbench: 14 } }).verdict).toBe("stale");
  });

  it("never reads a schema the reader did not report as a match", () => {
    const row = judge(owner, { reader: "old gitpulse-mcp", keys: [...ALL], schemas: { store: 27 } });
    expect(row.verdict).toBe("unverifiable");
    expect(row.detail).toContain("dc-store, workbench, devmap-extract");
  });

  it("calls a reader stale when one schema is behind even if another went unreported", () => {
    const row = judge(owner, { reader: "old pin", keys: [...ALL], schemas: { store: 23, dcStore: 9 } });
    expect(row.verdict).toBe("stale");
    expect(row.detail).toContain("devmap store 23, DevCouncil writes 27");
    expect(row.detail).toContain("does not report workbench");
  });

  it("calls a vendored extractor at 87 stale against DevCouncil's 94, and a match at 94", () => {
    const stale = judge(owner, { reader: "GitPulse vendored crates", keys: [...ALL], schemas: { ...owner, extract: 87 } });
    expect(stale.verdict).toBe("stale");
    expect(stale.detail).toContain("devmap-extract 87, DevCouncil writes 94");
    expect(judge(owner, { reader: "GitPulse vendored crates", keys: [...ALL], schemas: { ...owner } }).verdict).toBe("match");
  });

  it("calls a consumer that cannot report its extract version unverifiable, and the summary fails", () => {
    const { extract: _omitted, ...withoutExtract } = owner;
    const row = judge(owner, { reader: "old gitpulse-mcp", keys: [...ALL], schemas: withoutExtract });
    expect(row.verdict).toBe("unverifiable");
    expect(row.detail).toContain("does not report devmap-extract");
    expect(summarize([judge(owner, { reader: "ok", keys: [...ALL], schemas: { ...owner } }), row]).ok).toBe(false);
  });

  it("keeps a probe that failed apart from one that is not installed", () => {
    expect(judge(owner, { reader: "r", keys: [...ALL], error: "no handshake" }).verdict).toBe("unverifiable");
    expect(judge(owner, { reader: "r", keys: [...ALL], absent: "not on PATH" }).verdict).toBe("absent");
  });

  it("compares only the keys a reader is asked for", () => {
    expect(judge(owner, { reader: "dcstore", keys: ["workbench"], schemas: { workbench: 13 } }).verdict).toBe("match");
  });
});

describe("devmapReading", () => {
  const cli = (extract: number | null) => ({ available: true as const, expected: 27, versionLine: "devmap 0.2.4", source: "version", extract });

  it("is unverifiable for extract while devmap --version does not print it, and says what to change", () => {
    const row = judge(owner, devmapReading(cli(null)));
    expect(row.verdict).toBe("unverifiable");
    expect(row.detail).toContain("does not report devmap-extract");
    expect(row.detail).toContain("DevCouncil must add it");
  });

  it("matches once devmap prints the extraction schema, and is stale when it is behind", () => {
    expect(judge(owner, devmapReading(cli(94))).verdict).toBe("match");
    expect(judge(owner, devmapReading(cli(93))).verdict).toBe("stale");
  });
});

describe("summarize", () => {
  const row = (verdict: "match" | "stale" | "unverifiable" | "absent") => ({ reader: verdict, verdict, detail: "" });

  it("passes only when every present reader matches", () => {
    expect(summarize([row("match"), row("absent")])).toEqual({ ok: true, summary: "OK: 1 match · 1 absent (not checked)" });
    expect(summarize([row("match"), row("stale")]).ok).toBe(false);
  });

  it("fails a reader that could not be checked: it never exits like one that matched", () => {
    expect(summarize([row("match"), row("unverifiable")]).ok).toBe(false);
  });

  it("fails a run in which nothing was actually checked", () => {
    expect(summarize([row("absent"), row("absent")]).ok).toBe(false);
    expect(summarize([]).summary).toBe("FAIL: nothing checked");
  });
});

describe("pins", () => {
  const sha = "a".repeat(40);

  it("reads Manvi's DevCouncil pin from its tab-separated pins file", () => {
    const text = `# comment\nDevCouncil\thttps://x/DevCouncil.git\t${sha}\ngusset\thttps://x/gusset.git\t${"b".repeat(40)}\n`;
    expect(manviPin(text)).toBe(sha);
    expect(manviPin("gusset\tu\t" + sha)).toBeNull();
  });

  it("reads Jarvis's DevCouncil pin and refuses anything that is not a full sha", () => {
    expect(jarvisPin(JSON.stringify({ devcouncil: { revision: sha } }))).toBe(sha);
    expect(jarvisPin(JSON.stringify({ devcouncil: { revision: "main" } }))).toBeNull();
  });

  it("reads the constants at the pinned commit, not at HEAD", () => {
    const { root, old, bump } = devcouncilFixture();
    expect(schemasAt(root, old)).toEqual({ ...owner, store: 26 });
    expect(schemasAt(root, bump)).toEqual(owner);
  });

  it("calls a pin whose schemas DevCouncil has since moved stale", () => {
    const { root, old, head } = devcouncilFixture();
    const consumer = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-consumer-"));
    tempDirs.push(consumer);
    writeFileSync(path.join(consumer, "upstream.lock.json"), JSON.stringify({ devcouncil: { revision: old } }));
    const row = judge(owner, pinReading("Jarvis pin", root, head, consumer, "upstream.lock.json", jarvisPin));
    expect(row.verdict).toBe("stale");
    expect(row.detail).toContain("devmap store 26, DevCouncil writes 27");
  });

  it("calls a pin behind HEAD with the same schemas a match, and says it is behind", () => {
    const { root, bump, head } = devcouncilFixture();
    const consumer = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-consumer-"));
    tempDirs.push(consumer);
    writeFileSync(path.join(consumer, "upstream.lock.json"), JSON.stringify({ devcouncil: { revision: bump } }));
    const row = judge(owner, pinReading("Jarvis pin", root, head, consumer, "upstream.lock.json", jarvisPin));
    expect(row.verdict).toBe("match");
    expect(row.detail).toContain(`pins ${bump.slice(0, 8)}, DevCouncil HEAD is ${head.slice(0, 8)}`);
  });

  it("reports a pin the local clone does not have as unverifiable, not stale or matching", () => {
    const { root, head } = devcouncilFixture();
    const consumer = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-consumer-"));
    tempDirs.push(consumer);
    writeFileSync(path.join(consumer, "upstream.lock.json"), JSON.stringify({ devcouncil: { revision: "c".repeat(40) } }));
    const row = judge(owner, pinReading("Jarvis pin", root, head, consumer, "upstream.lock.json", jarvisPin));
    expect(row.verdict).toBe("unverifiable");
    expect(row.detail).toMatch(/fetch it/);
  });

  it("reads a pin from before the extract constant existed as unverifiable for it, not a crash", () => {
    const { root, old } = devcouncilFixture();
    writeFileSync(path.join(root, "rust", SCHEMAS.extract.file), "// no constant yet\n");
    git(root, "add", "-A");
    git(root, "commit", "-q", "-m", "drop extract");
    const row = judge(owner, { reader: "Manvi pin", keys: [...ALL], schemas: schemasAt(root, git(root, "rev-parse", "HEAD")) });
    expect(row.verdict).toBe("unverifiable");
    expect(schemasAt(root, old)).toHaveProperty("extract", 94);
  });

  it("reads a pin from before a schema was versioned as stale on what it did declare", () => {
    const root = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-early-"));
    tempDirs.push(root);
    git(root, "init", "-q");
    git(root, "config", "user.email", "fixture@example.invalid");
    git(root, "config", "user.name", "fixture");
    git(root, "config", "commit.gpgsign", "false");
    writeOwner(root, { store: 23, dcStore: 9, workbench: 0, extract: 94 });
    // Before the workbench schema existed: no constant in its file.
    writeFileSync(path.join(root, "rust", SCHEMAS.workbench.file), "const SCHEMA: &str = \"\";\n");
    git(root, "add", "-A");
    git(root, "commit", "-q", "-m", "early");
    const early = git(root, "rev-parse", "HEAD");
    expect(schemasAt(root, early)).toEqual({ store: 23, dcStore: 9, extract: 94 });
    const row = judge(owner, { reader: "Jarvis pin", keys: [...ALL], schemas: schemasAt(root, early) });
    expect(row.verdict).toBe("stale");
  });

  it("reports a consumer that is not checked out here as absent", () => {
    const row = judge(owner, pinReading("Jarvis pin", "/nowhere", "", null, "upstream.lock.json", jarvisPin));
    expect(row.verdict).toBe("absent");
  });
});

describe("contractRow", () => {
  function fixture(ours: string, theirs: string | null) {
    const root = mkdtempSync(path.join(tmpdir(), "gitpulse-schema-contract-"));
    tempDirs.push(root);
    const copy = path.join(root, "ours.json");
    writeFileSync(copy, ours);
    const owner = path.join(root, "manvi");
    mkdirSync(path.join(owner, "contracts"), { recursive: true });
    if (theirs !== null) writeFileSync(path.join(owner, SERVE_CONTRACT), theirs);
    return { copy, owner };
  }

  it("matches an identical copy", () => {
    const { copy, owner } = fixture("{}\n", "{}\n");
    expect(contractRow("serve", copy, owner).verdict).toBe("match");
  });

  it("calls a copy the owner has moved past stale", () => {
    const { copy, owner } = fixture("{}\n", '{"withholds_provider_credentials":true}\n');
    expect(contractRow("serve", copy, owner).verdict).toBe("stale");
  });

  it("keeps a missing owner file apart from a missing owner checkout", () => {
    const { copy, owner } = fixture("{}\n", null);
    expect(contractRow("serve", copy, owner).verdict).toBe("unverifiable");
    expect(contractRow("serve", copy, null).verdict).toBe("absent");
  });
});

describe("parseArgs", () => {
  it("accepts the documented flags and refuses anything else", () => {
    expect(parseArgs(["--json", "--app", "/x/GitPulse.app", "--timeout", "500"])).toEqual({ app: "/x/GitPulse.app", timeoutMs: 500, json: true });
    expect(parseArgs(["--timeout", "0"])).toHaveProperty("error");
    expect(parseArgs(["--bogus"])).toHaveProperty("error");
  });
});
