import { describe, expect, it } from "vitest";
import { linkCandidatesHonesty } from "./linkCandidates";

describe("linkCandidatesHonesty", () => {
  it("explains an empty registry", () => {
    expect(
      linkCandidatesHonesty({ links: [], count: 0, repos_considered: 0 }),
    ).toContain("No repos");
  });

  it("explains zero candidates with repos present", () => {
    expect(
      linkCandidatesHonesty({ links: [], count: 0, repos_considered: 3 }),
    ).toContain("No cross-repo");
  });

  it("names a partial sample when links are shorter than count", () => {
    expect(
      linkCandidatesHonesty({
        links: [
          {
            from_repo: "a",
            from_file: "a.ts",
            module_specifier: "@b/x",
            to_repo: "b",
            evidence: "import",
          },
        ],
        count: 4,
        repos_considered: 2,
      }),
    ).toBe("Showing 1 of 4 candidates across 2 repo(s).");
  });
});
