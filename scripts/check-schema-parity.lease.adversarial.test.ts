import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { SCHEMAS, devmapReading, judge, rustConstant, schemasAt, summarize } from "./check-schema-parity.mjs";
import { readCliSchema } from "./check-vendor-schema.mjs";

const C = "STACK_FORMAT_VERSION";
const owner = { store: 27, dcStore: 9, workbench: 13, extract: 94, lease: 2 };

describe("lease rustConstant shapes", () => {
  it("reads u8/u32 pub const, refuses pub(crate), underscores, hex, expressions, missing", () => {
    expect(rustConstant(`pub const ${C}: u32 = 2;`, C)).toBe(2);
    expect(rustConstant(`pub const ${C}: u8 = 2;`, C)).toBe(2);
    expect(rustConstant(`pub(crate) const ${C}: u32 = 2;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 1_0;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 0x2;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 1 + 1;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 2 + 0;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = 2u32;`, C)).toBeNull();
    expect(rustConstant(`pub const ${C}: u32 = -2;`, C)).toBeNull();
    expect(rustConstant(`pub const OTHER: u32 = 2;`, C)).toBeNull();
    expect(rustConstant(``, C)).toBeNull();
  });
  it("two cfg-gated definitions differing -> null; equal -> value", () => {
    expect(rustConstant(`#[cfg(a)]\npub const ${C}: u32 = 1;\n#[cfg(not(a))]\npub const ${C}: u32 = 2;`, C)).toBeNull();
  });
  it("cfg(test) block definition differing from the real one is not silently taken", () => {
    const t = `pub const ${C}: u32 = 2;\n#[cfg(test)]\nmod t {\n pub const ${C}: u32 = 1;\n}`;
    expect(rustConstant(t, C)).toBeNull();
  });
  it("lease row at a pin predating stacked.rs: key left out, judged unverifiable not match/crash", () => {
    const run = ((_c: string, a: readonly string[]) => a[0] === "cat-file" ? { status: 0, stdout: "" } : { status: 128, stdout: "" }) as never;
    const got = schemasAt("/x", "a".repeat(40), run);
    expect(got.lease).toBeUndefined();
    expect(judge(owner, { reader: "r", keys: ["store", "dcStore", "workbench", "extract", "lease"], schemas: got }).verdict).toBe("unverifiable");
  });
  it("git show spawn error for lease file throws (reported as row error), not a match", () => {
    const run = ((_c: string, a: readonly string[]) => a[0] === "cat-file" ? { status: 0, stdout: "" } : { error: new Error("boom") }) as never;
    expect(() => schemasAt("/x", "a".repeat(40), run)).toThrow();
  });
});

describe("lease judging", () => {
  const keys = ["store", "extract", "lease"] as const;
  const base = { store: 27, extract: 94 };
  it("reader N, writer N+1 stale; equal match; reader ahead is stale too", () => {
    expect(judge(owner, { reader: "r", keys: [...keys], schemas: { ...base, lease: 1 } }).verdict).toBe("stale");
    expect(judge(owner, { reader: "r", keys: [...keys], schemas: { ...base, lease: 2 } }).verdict).toBe("match");
    expect(judge(owner, { reader: "r", keys: [...keys], schemas: { ...base, lease: 3 } }).verdict).toBe("stale");
  });
  it("lease unreported -> unverifiable; any unverifiable fails overall", () => {
    expect(judge(owner, { reader: "r", keys: [...keys], schemas: base }).verdict).toBe("unverifiable");
    expect(summarize([{ verdict: "match" }, { verdict: "unverifiable" }] as never).ok).toBe(false);
  });
  it("devmapReading lists lease and a null lease is not a match", () => {
    const r = devmapReading({ available: true, expected: 27, extract: 94, lease: null, versionLine: "x", source: "version" } as never);
    expect(r.keys).toContain("lease");
    expect(judge(owner, r).verdict).toBe("unverifiable");
  });
});

describe.skipIf(process.platform === "win32")("readCliSchema lease parsing", () => {
  const dirs: string[] = [];
  afterEach(async () => { await Promise.all(dirs.splice(0).map((d) => rm(d, { recursive: true, force: true }))); });
  async function bin(body: string, doctor = true) {
    const dir = await mkdtemp(path.join(tmpdir(), "adv-lease-"));
    dirs.push(dir);
    const b = path.join(dir, "devmap");
    await writeFile(b, `#!/bin/sh
if [ "$1" = "doctor" ]; then ${doctor ? `echo '{"expected_schema_version":27}'; exit 0` : "exit 1"}; fi
if [ "$1" = "status" ]; then echo '{}'; exit 0; fi
if [ "$1" = "--version" ]; then ${body}; fi
`);
    await chmod(b, 0o755);
    return b;
  }
  for (const doctor of [true, false]) {
    const tag = doctor ? "doctor" : "version";
    it(`[${tag}] absent -> null`, async () => {
      expect((readCliSchema(await bin(`echo "devmap 0.2.5 (store schema 27)"`, doctor)) as any).lease).toBeNull();
    });
    it(`[${tag}] '-1' -> null`, async () => {
      expect((readCliSchema(await bin(`echo "devmap 0.2.5 (store schema 27, stack lease format -1)"`, doctor)) as any).lease).toBeNull();
    });
    it(`[${tag}] huge value never judged as a match`, async () => {
      const c = readCliSchema(await bin(`echo "devmap 0.2.5 (store schema 27, stack lease format 99999999999999999999)"`, doctor)) as any;
      expect(judge(owner, devmapReading({ ...c, extract: 94 })).verdict).not.toBe("match");
    });
    it(`[${tag}] CRLF / uppercase / extra spaces`, async () => {
      expect((readCliSchema(await bin(`printf 'devmap (store schema 27, stack lease format 2)\\r\\n'`, doctor)) as any).lease).toBe(2);
      expect((readCliSchema(await bin(`echo "devmap (store schema 27, STACK LEASE FORMAT 2)"`, doctor)) as any).lease).toBe(2);
      expect((readCliSchema(await bin(`echo "devmap (store schema 27, stack lease format  2)"`, doctor)) as any).lease).toBeNull();
    });
    it(`[${tag}] stderr-only -> null`, async () => {
      expect((readCliSchema(await bin(`echo "stack lease format 2" >&2`, doctor)) as any).lease ?? null).toBeNull();
    });
  }
  it("[doctor] nonzero exit with valid-looking line -> null", async () => {
    expect((readCliSchema(await bin(`echo "stack lease format 2"; exit 3`)) as any).lease).toBeNull();
  });
  it("[version] nonzero exit -> unavailable, no lease", async () => {
    const c = readCliSchema(await bin(`echo "store schema 27 stack lease format 2"; exit 3`, false)) as any;
    expect(c.available).toBe(false);
  });
  it("missing bin -> unavailable, no throw", () => {
    expect((readCliSchema("/nonexistent/devmap") as any).available).toBe(false);
  });
  it("non-executable bin -> unavailable, no throw", async () => {
    const dir = await mkdtemp(path.join(tmpdir(), "adv-lease-"));
    dirs.push(dir);
    const b = path.join(dir, "devmap");
    await writeFile(b, "#!/bin/sh\necho hi\n");
    await chmod(b, 0o644);
    expect((readCliSchema(b) as any).available).toBe(false);
  });
  it("hang in --version is cut off", async () => {
    const b = await bin(`sleep 60`);
    const t = Date.now();
    expect((readCliSchema(b) as any).lease).toBeNull();
    expect(Date.now() - t).toBeLessThan(25_000);
  }, 40_000);
});
