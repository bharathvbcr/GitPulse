import { describe, expect, it } from "vitest";
import {
  releaseTagSuggestion,
  summarizeCommitReview,
  type CommitReviewReport,
} from "./model";

describe("MANVI ops helpers", () => {
  it("suggests the next stable patch without being confused by prereleases", () => {
    expect(releaseTagSuggestion(["v1.2.9", "v1.3.0-beta.1", "not-a-release"])).toBe("v1.2.10");
    expect(releaseTagSuggestion([])).toBe("v0.1.0");
  });

  it("keeps capped commit review coverage explicit", () => {
    const report: CommitReviewReport = {
      range: "origin/main..HEAD",
      total_commits: 620,
      reviewed_commits: 500,
      truncated: true,
      conventional_commits: 410,
      issue_linked_commits: 120,
      findings: [],
    };

    expect(summarizeCommitReview(report)).toContain("500 of 620");
    expect(summarizeCommitReview(report)).toContain("capped");
  });

  it("reports complete review coverage without a capped warning", () => {
    const report: CommitReviewReport = {
      range: "main..HEAD",
      total_commits: 3,
      reviewed_commits: 3,
      truncated: false,
      conventional_commits: 3,
      issue_linked_commits: 2,
      findings: [],
    };

    expect(summarizeCommitReview(report)).toBe("Reviewed all 3 outgoing commits.");
  });
});
