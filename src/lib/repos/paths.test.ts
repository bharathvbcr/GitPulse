import { describe, expect, it } from "vitest";
import {
  disambiguateLabels,
  displayName,
  identityKey,
  isPathAmong,
  normalizeRepoPath,
  sameRepo,
} from "./paths";

describe("normalizeRepoPath", () => {
  it("rejects empty, control, and whitespace-only input", () => {
    expect(normalizeRepoPath("")).toBeNull();
    expect(normalizeRepoPath("   ")).toBeNull();
    expect(normalizeRepoPath("\0/tmp/repo")).toBeNull();
    expect(normalizeRepoPath("/tmp/\nrepo")).toBeNull();
    expect(normalizeRepoPath("/")).toBeNull();
  });

  it("strips trailing slashes, backslashes, and duplicate separators", () => {
    expect(normalizeRepoPath("/Users/acme/gitpulse/")).toBe("/Users/acme/gitpulse");
    expect(normalizeRepoPath("/Users//acme///gitpulse")).toBe("/Users/acme/gitpulse");
    expect(normalizeRepoPath("C:\\Users\\acme\\gitpulse\\")).toBe("C:/Users/acme/gitpulse");
  });

  it("keeps UNC prefix slashes", () => {
    expect(normalizeRepoPath("\\\\server\\share\\repo")).toBe("//server/share/repo");
    expect(normalizeRepoPath("//server//share/repo/")).toBe("//server/share/repo");
  });

  it("NFC-normalizes unicode names", () => {
    const nfd = "/tmp/cafe\u0301";
    const nfc = "/tmp/café";
    expect(normalizeRepoPath(nfd)).toBe(nfc);
  });
});

describe("identityKey", () => {
  const ci = { caseInsensitive: true };
  const cs = { caseInsensitive: false };

  it("treats trailing-slash and case variants as the same repo on case-insensitive volumes", () => {
    expect(sameRepo("/Users/Acme/GitPulse/", "/Users/acme/gitpulse", ci)).toBe(true);
    expect(identityKey("/Users/Acme/GitPulse", ci)).toBe("/users/acme/gitpulse");
  });

  it("keeps case on case-sensitive volumes", () => {
    expect(sameRepo("/tmp/GitPulse", "/tmp/gitpulse", cs)).toBe(false);
  });

  it("matches a path against an open-tab list by identity", () => {
    expect(isPathAmong("/Users/Acme/GitPulse/", ["/Users/acme/gitpulse", "/tmp/other"], ci)).toBe(true);
    expect(isPathAmong("/tmp/missing", ["/Users/acme/gitpulse"], ci)).toBe(false);
  });
});

describe("display names", () => {
  it("uses the last path component", () => {
    expect(displayName("/Users/acme/gitpulse")).toBe("gitpulse");
    expect(displayName("C:/src/gitpulse")).toBe("gitpulse");
  });

  it("disambiguates duplicate folder names with the parent", () => {
    const labels = disambiguateLabels([
      "/Users/acme/code/gitpulse",
      "/Users/acme/oss/gitpulse",
      "/tmp/unique",
    ]);
    expect(labels.get("/Users/acme/code/gitpulse")).toBe("code/gitpulse");
    expect(labels.get("/Users/acme/oss/gitpulse")).toBe("oss/gitpulse");
    expect(labels.get("/tmp/unique")).toBe("unique");
  });

  it("widens past an identical parent to the grandparent", () => {
    const labels = disambiguateLabels(["/a/x/y", "/b/x/y"]);
    expect(labels.get("/a/x/y")).toBe("a/x/y");
    expect(labels.get("/b/x/y")).toBe("b/x/y");
  });

  it("widens each colliding member only as far as needed", () => {
    const labels = disambiguateLabels([
      "/work/a/x/y",
      "/work/b/x/y",
      "/work/unique",
    ]);
    expect(labels.get("/work/a/x/y")).toBe("a/x/y");
    expect(labels.get("/work/b/x/y")).toBe("b/x/y");
    expect(labels.get("/work/unique")).toBe("unique");
  });

  it("keeps deepening until every member is unique", () => {
    const labels = disambiguateLabels(["/a/x/y", "/b/x/y", "/c/b/x/y"]);
    const values = [...labels.values()];
    expect(new Set(values).size).toBe(values.length);
    expect(labels.get("/a/x/y")).toBe("a/x/y");
    expect(labels.get("/b/x/y")).toBe("b/x/y");
    expect(labels.get("/c/b/x/y")).toBe("c/b/x/y");
  });

  it("falls back to a bare name for root-level members without a parent", () => {
    const labels = disambiguateLabels(["/repo", "/work/repo"]);
    expect(labels.get("/repo")).toBe("repo");
    expect(labels.get("/work/repo")).toBe("work/repo");
  });

  it("resolves deterministically regardless of input order", () => {
    const forward = disambiguateLabels(["/a/x/y", "/b/x/y"]);
    const backward = disambiguateLabels(["/b/x/y", "/a/x/y"]);
    expect(forward.get("/a/x/y")).toBe(backward.get("/a/x/y"));
    expect(forward.get("/b/x/y")).toBe(backward.get("/b/x/y"));
  });
});

describe("repo identity is a well-behaved equivalence relation", () => {
  // Tab de-duplication, "is this repo already open", and recent-list matching
  // all rest on sameRepo. If it is not reflexive, symmetric and transitive,
  // the same repository opens twice or two repositories share a tab.
  const CASE_SENSITIVE = { caseInsensitive: false };
  const CASE_INSENSITIVE = { caseInsensitive: true };

  const paths = [
    "/Users/a/repo",
    "/Users/a/repo/",
    "/Users/a//repo",
    "\\Users\\a\\repo",
    "/Users/a/repo///",
    "/Users/a/other",
    "/Users/A/Repo",
    "//server/share/repo",
    "/",
    "",
    "   ",
    "/Users/a/café",
    "/Users/a/cafe\u0301",
  ];

  for (const options of [CASE_SENSITIVE, CASE_INSENSITIVE]) {
    const label = options.caseInsensitive ? "case-insensitive" : "case-sensitive";

    it(`is reflexive (${label})`, () => {
      for (const path of paths) {
        const normalized = normalizeRepoPath(path);
        // An unusable path has no identity, so reflexivity applies to the rest.
        if (normalized) expect(sameRepo(path, path, options), path).toBe(true);
      }
    });

    it(`is symmetric (${label})`, () => {
      for (const a of paths) {
        for (const b of paths) {
          expect(sameRepo(a, b, options), `${a} vs ${b}`).toBe(sameRepo(b, a, options));
        }
      }
    });

    it(`is transitive (${label})`, () => {
      for (const a of paths) {
        for (const b of paths) {
          for (const c of paths) {
            if (sameRepo(a, b, options) && sameRepo(b, c, options)) {
              expect(sameRepo(a, c, options), `${a} ~ ${b} ~ ${c}`).toBe(true);
            }
          }
        }
      }
    });
  }

  it("never matches an unusable path against anything, including itself", () => {
    // "" has no identity; if identityKey's empty string compared equal, every
    // rejected path would look like the same repository as every other.
    for (const bad of ["", "   ", "/", "//", "\u0000", "a\u0007b"]) {
      for (const good of ["/Users/a/repo", "/Users/a/other", ...["", "   "]]) {
        expect(sameRepo(bad, good, CASE_SENSITIVE), `${bad} vs ${good}`).toBe(false);
      }
    }
  });

  it("normalizing is idempotent", () => {
    // Identity is derived from the normalized form, so a second pass changing
    // the answer would make equality depend on how often it was applied.
    for (const path of paths) {
      const once = normalizeRepoPath(path);
      if (once === null) continue;
      expect(normalizeRepoPath(once), path).toBe(once);
    }
  });

  it("treats composed and decomposed unicode as the same repository", () => {
    // macOS hands back decomposed names while a typed or stored path is often
    // composed; without NFC the same checkout would open as two tabs.
    expect(sameRepo("/Users/a/café", "/Users/a/cafe\u0301", CASE_SENSITIVE)).toBe(true);
  });

  it("keeps genuinely different repositories apart", () => {
    for (const [a, b] of [
      ["/Users/a/repo", "/Users/a/other"],
      ["/Users/a/repo", "/Users/a/repo2"],
      ["/Users/a/repo", "/Users/b/repo"],
      ["//server/share/repo", "//other/share/repo"],
    ] as const) {
      expect(sameRepo(a, b, CASE_SENSITIVE), `${a} vs ${b}`).toBe(false);
      expect(sameRepo(a, b, CASE_INSENSITIVE), `${a} vs ${b}`).toBe(false);
    }
  });
});
