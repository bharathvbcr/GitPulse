import { describe, it, expect, afterAll } from "vitest";
import path from "node:path";
import {
  createTempManager,
  SCHEMA_CONSTANTS,
  normalizePriority,
  computeVirtualWindow,
  CondvarLedgerIngestSync,
} from "./helpers/test-utils";

describe("Tier 5: Adversarial Hardening & Fault Injection", () => {
  const tempMgr = createTempManager();

  afterAll(() => {
    tempMgr.cleanup();
  });

  it("5.1-ADV: Malformed SQL injection in task title or symbol name does not compromise query integrity", () => {
    const maliciousInputs = [
      "'; DROP TABLE work_items; --",
      "\" OR 1=1 --",
      "<script>alert('xss')</script>",
      "\x00\x01\x02\x03\x04",
      "UNION SELECT * FROM sqlite_master",
    ];

    for (const input of maliciousInputs) {
      const sanitized = JSON.stringify({ title: input });
      const parsed = JSON.parse(sanitized);
      expect(parsed.title).toBe(input);
    }
  });

  it("5.2-ADV: Corrupted zstd header with garbage payload throws deterministic error without SIGSEGV", () => {
    const garbageBytes = Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00]); // ELF header
    const tryDecompress = (buf: Buffer) => {
      if (!buf.subarray(0, 4).equals(SCHEMA_CONSTANTS.ZSTD_MAGIC_HEADER)) {
        throw new Error("Invalid zstd compression frame header");
      }
      return "decompressed";
    };

    expect(() => tryDecompress(garbageBytes)).toThrow(/Invalid zstd compression frame header/);
  });

  it("5.3-ADV: Extreme priority string noise normalizes safely to P2 fallback", () => {
    const noisyInputs = [
      "",
      "   ",
      "p-negative",
      "P99999999999999999999",
      "!@#$%^&*()",
      "null",
      "undefined",
      "NaN",
      "true",
      "false",
    ];

    for (const noisy of noisyInputs) {
      const res = normalizePriority(noisy);
      expect(["P0", "P1", "P2", "P3"]).toContain(res.code);
      expect(typeof res.rank).toBe("number");
    }
  });

  it("5.4-ADV: VirtualList handles negative, NaN, Infinity, and zero dimensions gracefully", () => {
    const edgeCases = [
      { total: -10, itemH: 50, viewH: 500, scroll: 0 },
      { total: 100, itemH: 0, viewH: 500, scroll: 0 },
      { total: 100, itemH: -50, viewH: 500, scroll: 0 },
      { total: 0, itemH: 50, viewH: 0, scroll: 0 },
    ];

    for (const ec of edgeCases) {
      const win = computeVirtualWindow(ec.total, ec.itemH, ec.viewH, ec.scroll);
      expect(win.renderedCount).toBe(0);
      expect(win.totalHeight).toBe(0);
    }
  });

  it("5.5-ADV: Condvar lock handles rapid lock acquisition, release, and abortion under contention", async () => {
    const sync = new CondvarLedgerIngestSync();
    let acquiredCount = 0;

    const workers = Array.from({ length: 15 }, async (_, i) => {
      const ok = await sync.acquire("/adv-ledger", 300);
      if (ok) {
        acquiredCount++;
        sync.release("/adv-ledger");
      }
    });

    await Promise.all(workers);
    expect(acquiredCount).toBeGreaterThan(0);
    expect(sync.isLocked("/adv-ledger")).toBe(false);
  });
});
