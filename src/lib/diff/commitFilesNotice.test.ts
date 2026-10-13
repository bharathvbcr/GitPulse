import { describe, expect, it } from "vitest";
import { summarizeCommitFiles } from "./commitFilesNotice";
import type { CommitDetails, CommitFileChange } from "../stores/graphStore";

function files(count: number): CommitFileChange[] {
  return Array.from({ length: count }, (_, n) => ({
    path: `f${n}.txt`,
    status_code: "M",
    additions: 1,
    deletions: 0,
  }));
}

type Input = Pick<
  CommitDetails,
  "changed_files" | "files_total_count" | "files_list_truncated" | "numstat_truncation_reason"
>;

describe("summarizeCommitFiles", () => {
  it("says nothing about a commit read whole", () => {
    const summary = summarizeCommitFiles({ changed_files: files(3) } as Input);
    expect(summary).toEqual({ count: "3", partial: false, note: null });
  });

  it("is empty before the details land", () => {
    expect(summarizeCommitFiles(null)).toEqual({ count: "0", partial: false, note: null });
  });

  it("reports the true count when only the list was capped", () => {
    const summary = summarizeCommitFiles({
      changed_files: files(5),
      files_list_truncated: true,
      files_total_count: 12000,
    } as Input);
    expect(summary.count).toBe("12,000");
    expect(summary.partial).toBe(false);
    expect(summary.note).toBe("Only the first 5 of 12,000 files are listed.");
  });

  it("never shows a cut git stream as an exact count or exact totals", () => {
    const summary = summarizeCommitFiles({
      changed_files: files(40),
      files_list_truncated: true,
      numstat_truncation_reason: "git output exceeded 4 MB",
    } as Input);
    expect(summary.count).toBe("40+");
    expect(summary.partial).toBe(true);
    expect(summary.note).toContain("Git output exceeded 4 MB");
    expect(summary.note).toContain("totals are partial");
  });

  it("prefers the cut over a list cap when both apply", () => {
    const summary = summarizeCommitFiles({
      changed_files: files(5000),
      files_list_truncated: true,
      numstat_truncation_reason: "git output exceeded 4 MB",
    } as Input);
    expect(summary.count).toBe("5,000+");
    expect(summary.partial).toBe(true);
  });
});
