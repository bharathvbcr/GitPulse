import { describe, expect, it } from "vitest";
import { linkCandidatesHonesty } from "./linkCandidates";

describe("linkCandidatesHonesty", () => {
  it("explains an empty registry", () => {
    expect(
      linkCandidatesHonesty({ links: [], count: 0, repos_considered: 0, coverage_gaps: [] }),
    ).toContain("No repos");
  });

  it("explains zero candidates with repos present", () => {
    expect(
      linkCandidatesHonesty({ links: [], count: 0, repos_considered: 3, coverage_gaps: [] }),
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
        coverage_gaps: [],
      }),
    ).toBe("Showing 1 of 4 candidates across 2 repo(s).");
  });

  it("never reads an empty list as none when something could not be read", () => {
    const text = linkCandidatesHonesty({
      links: [],
      count: 0,
      repos_considered: 2,
      coverage_gaps: [{ repo: "b", reason: "store would not open" }],
    });
    expect(text).toContain("could not be read");
    expect(text).toContain("b: store would not open");
    expect(text).toContain("incomplete");
  });
});
