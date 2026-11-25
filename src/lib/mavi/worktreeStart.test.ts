import { describe, expect, it } from "vitest";
import { planWorktree, slugFromMessage } from "./worktreeStart";

describe("Mavi worktree plans", () => {
  it("names a sibling worktree on a mavi/ branch", () => {
    expect(planWorktree("/Users/me/code/app/", "fix-retry-ab12")).toEqual({
      path: "/Users/me/code/app.wt-fix-retry-ab12",
      branch: "mavi/fix-retry-ab12",
      slug: "fix-retry-ab12",
    });
    expect(planWorktree("C:\\code\\app", "x1")?.path).toBe("C:\\code\\app.wt-x1");
    expect(planWorktree("/app", "x")).toBeNull();
  });

  it("derives a short slug from the message", () => {
    const slug = slugFromMessage("Make the retry back off, please!", 1_700_000_000_000);
    expect(slug).toMatch(/^make-the-retry-back-[a-z0-9]{4}$/);
    expect(slugFromMessage("??", 1)).toMatch(/^thread-/);
  });
});
