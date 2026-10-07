import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./CoverageAgentPrompt.svelte", import.meta.url), "utf8");

describe("CoverageAgentPrompt", () => {
  it("finds the repository's agent session under the shared path identity, not an exact string", () => {
    // A session launched from a tab spelled /Code/App was invisible to the
    // prompt on /code/app: "View agent session" never appeared.
    expect(source).toContain("sameRepo(session.repoPath, repoPath, pathOptions)");
    expect(source).not.toMatch(/session\.repoPath === repoPath/);
  });
});
