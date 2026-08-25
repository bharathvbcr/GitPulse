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
