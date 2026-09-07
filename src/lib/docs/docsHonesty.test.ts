import { describe, expect, it } from "vitest";
import {
  brokenLinksHonesty,
  docsSearchHonesty,
  docsStatusHonesty,
} from "./docsHonesty";

describe("docsStatusHonesty", () => {
  it("returns null when the vault is complete", () => {
    expect(
      docsStatusHonesty({
        noteCount: 4,
        truncated: false,
        skippedOversized: 0,
        skippedUnreadable: 0,
      }),
    ).toBeNull();
  });

  it("names vault caps and skips", () => {
    const line = docsStatusHonesty({
      noteCount: 5000,
      truncated: true,
      skippedOversized: 2,
      skippedUnreadable: 1,
    });
    expect(line).toContain("Vault capped");
    expect(line).toContain("5000");
    expect(line).toContain("not the whole docs set");
    expect(line).toContain("2 oversized");
    expect(line).toContain("1 unreadable");
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
