import { describe, expect, it } from "vitest";
import type { DocRenameOutcome } from "./client";
import {
  brokenLinksHonesty,
  docsRenameHonesty,
  docsSearchHonesty,
  docsStatusHonesty,
} from "./docsHonesty";

describe("docsStatusHonesty", () => {
  it("does not mislabel every budget cap as a file-count overflow", () => {
    const line = docsStatusHonesty({ noteCount: 32, truncated: true, skippedOversized: 0, skippedUnreadable: 0, notScanned: 0 });
    expect(line).toContain("resource limit");
    expect(line).not.toContain("file list exceeded");
  });
  it("returns null when the vault is complete", () => {
    expect(
      docsStatusHonesty({
        noteCount: 4,
        truncated: false,
        skippedOversized: 0,
        skippedUnreadable: 0,
        notScanned: 0,
      }),
    ).toBeNull();
  });

  it("names vault caps and skips", () => {
    const line = docsStatusHonesty({
      noteCount: 5000,
      truncated: true,
      skippedOversized: 2,
      skippedUnreadable: 1,
      notScanned: 1,
    });
    expect(line).toContain("Vault capped");
    expect(line).toContain("5000");
    expect(line).toContain("not the whole docs set");
    expect(line).toContain("2 oversized");
    expect(line).toContain("1 unreadable");
  });
});

describe("docsRenameHonesty", () => {
  const scan = { noteCount: 2, truncated: false, skippedOversized: 0, skippedUnreadable: 0, notScanned: 0 };
  const clean: DocRenameOutcome = {
    from: "a.md",
    to: "b.md",
    rewrittenPaths: ["c.md"],
    linksRewritten: 1,
    complete: true,
    skippedConflict: [],
    failed: [],
    leftBroken: [],
    scan,
  };

  it("says nothing about a rename that covered everything", () => {
    expect(docsRenameHonesty(clean)).toBeNull();
    expect(docsRenameHonesty(null)).toBeNull();
  });

  it("says how many notes were not scanned", () => {
    const line = docsRenameHonesty({ ...clean, scan: { ...scan, truncated: true, notScanned: 3 } });
    expect(line).toContain("3 notes were not scanned");
  });

  it("names conflicts, failures and links left behind, and says the rename is incomplete", () => {
    const line = docsRenameHonesty({
      ...clean,
      complete: false,
      skippedConflict: ["docs/Guide.md"],
      failed: [{ path: "docs/B.md", error: "disk full" }],
      leftBroken: [{ source: "docs/Ref.md", target: "Roadmap.md" }],
    });
    expect(line).toContain("incomplete");
    expect(line).toContain("1 note(s) rewritten");
    expect(line).toContain("docs/Guide.md");
    expect(line).toContain("docs/B.md (disk full)");
    expect(line).toContain("docs/Ref.md -> Roadmap.md");
  });
});

describe("docsSearchHonesty", () => {
  it("flags a full limit page as incomplete", () => {
    const hits = Array.from({ length: 50 }, (_, i) => ({
      path: `n${i}.md`,
      title: "t",
      context: "c",
      line: 1,
      score: 1,
    }));
    expect(docsSearchHonesty(hits, 50)).toContain("limit 50");
  });

  it("stays quiet under the limit", () => {
    expect(
      docsSearchHonesty([{ path: "a.md", title: "a", context: "", line: 1, score: 1 }], 50),
    ).toBeNull();
  });
});

describe("docsRenameHonesty (adversarial)", () => {
  const scan = { noteCount: 2, truncated: false, skippedOversized: 0, skippedUnreadable: 0, notScanned: 0 };
  const base: DocRenameOutcome = {
    from: "a.md",
    to: "b.md",
    rewrittenPaths: [],
    linksRewritten: 0,
    complete: true,
    skippedConflict: [],
    failed: [],
    leftBroken: [],
    scan,
  };

  it("uses the singular for exactly one unscanned note", () => {
    const line = docsRenameHonesty({ ...base, scan: { ...scan, notScanned: 1 } });
    expect(line).toContain("1 note was not scanned");
  });

  it("keeps the line bounded when thousands of links are left behind", () => {
    const leftBroken = Array.from({ length: 5000 }, (_, i) => ({
      source: `notes/n${i}.md`,
      target: "old.md",
    }));
    const line = docsRenameHonesty({ ...base, leftBroken });
    expect(line).not.toBeNull();
    expect((line ?? "").length).toBeLessThan(2000);
    expect(line).toContain("5000");
  });

  it("names a failed rewrite even when the complete flag disagrees", () => {
    const line = docsRenameHonesty({
      ...base,
      complete: true,
      failed: [{ path: "x.md", error: "disk full" }],
    });
    expect(line).toContain("x.md");
  });
});

describe("brokenLinksHonesty", () => {
  it("caps the displayed list and says so", () => {
    const links = Array.from({ length: 5 }, (_, i) => ({
      source: `s${i}.md`,
      target: `t${i}`,
    }));
    const { shown, honesty } = brokenLinksHonesty(links, 3);
    expect(shown).toHaveLength(3);
    expect(honesty).toBe("Showing 3 of 5 broken links.");
  });
});
