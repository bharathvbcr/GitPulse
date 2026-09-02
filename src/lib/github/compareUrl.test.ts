import { describe, expect, it } from "vitest";
import { isCompareRef, pullRequestCreateUrl } from "./compareUrl";

describe("isCompareRef", () => {
  it("accepts ordinary branch names, including slashes", () => {
    expect(isCompareRef("main")).toBe(true);
    expect(isCompareRef("feat/x")).toBe(true);
    expect(isCompareRef("claude/session-1")).toBe(true);
  });

  it("rejects anything that would break GitHub's compare delimiter", () => {
    expect(isCompareRef("")).toBe(false);
    expect(isCompareRef("feat x")).toBe(false);
    expect(isCompareRef("a...b")).toBe(false);
    expect(isCompareRef("main?expand=1")).toBe(false);
    expect(isCompareRef("main#diff")).toBe(false);
    expect(isCompareRef("feat\nmain")).toBe(false);
  });
});

describe("pullRequestCreateUrl", () => {
  it("builds the compare URL GitHub's new-PR form reads", () => {
    expect(pullRequestCreateUrl("https://github.com/acme/app", "main", "feat/x")).toBe(
      "https://github.com/acme/app/compare/main...feat%2Fx?expand=1",
    );
  });

  it("strips a trailing slash on the repo URL", () => {
    expect(pullRequestCreateUrl("https://github.com/acme/app/", "main", "feat")).toContain(
      "https://github.com/acme/app/compare/",
    );
  });

  it("returns null when the URL would name the wrong thing", () => {
    expect(pullRequestCreateUrl("", "main", "feat")).toBeNull();
    expect(pullRequestCreateUrl("https://github.com/acme/app", "main", "main")).toBeNull();
    expect(pullRequestCreateUrl("https://github.com/acme/app", "", "feat")).toBeNull();
    expect(pullRequestCreateUrl("javascript:alert(1)", "main", "feat")).toBeNull();
    expect(pullRequestCreateUrl("https://github.com/acme/app", "a...b", "feat")).toBeNull();
    expect(pullRequestCreateUrl("http://github.com/acme/app", "main", "feat")).toBeNull();
  });
});
