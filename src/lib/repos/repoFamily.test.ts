import { describe, expect, it } from "vitest";
import { checkoutName, familyFromCommonDir, familyLabels, familyName } from "./repoFamily";
import { identityKey } from "./paths";

const sensitive = { caseInsensitive: false };
const folding = { caseInsensitive: true };

describe("familyFromCommonDir", () => {
  it("names the primary checkout's directory as the root of an ordinary repository", () => {
    expect(familyFromCommonDir("/Users/me/Code/GitPulse/.git", sensitive)).toEqual({
      key: "/Users/me/Code/GitPulse/.git",
      root: "/Users/me/Code/GitPulse",
    });
  });

  it("keeps a bare repository's own directory as its root", () => {
    expect(familyFromCommonDir("/srv/git/api.git", sensitive)).toEqual({
      key: "/srv/git/api.git",
      root: "/srv/git/api.git",
    });
  });

  it("gives one key to every spelling of the same common directory", () => {
    const a = familyFromCommonDir("/Users/me/GitPulse/.git/", folding);
    const b = familyFromCommonDir("/users/ME//GitPulse/.GIT", folding);
    expect(a?.key).toBe(b?.key);
    // A case-sensitive volume keeps them apart: two directories, two families.
    expect(familyFromCommonDir("/a/R/.git", sensitive)?.key).not.toBe(
      familyFromCommonDir("/a/r/.git", sensitive)?.key,
    );
  });

  it("normalizes Windows and UNC paths without losing the prefix", () => {
    expect(familyFromCommonDir("C:\\code\\api\\.git", sensitive)?.root).toBe("C:/code/api");
    expect(familyFromCommonDir("//server/share/repo/.git", sensitive)?.root).toBe("//server/share/repo");
  });

  it("refuses anything that is not a readable path rather than inventing a family", () => {
    for (const bad of [null, undefined, 42, {}, "", "   ", "/", "//", "/a\u0000b/.git", "\n"]) {
      expect(familyFromCommonDir(bad, sensitive)).toBeNull();
    }
  });

  it("treats a bare `.git` at the filesystem root as a directory, not a parent", () => {
    // "/.git" has no parent worth naming; the directory itself is the root.
    expect(familyFromCommonDir("/.git", sensitive)).toEqual({ key: "/.git", root: "/.git" });
  });

  it("does not cut a directory that merely ends in .git-like text", () => {
    expect(familyFromCommonDir("/repos/legit", sensitive)?.root).toBe("/repos/legit");
    expect(familyFromCommonDir("/repos/x/.github", sensitive)?.root).toBe("/repos/x/.github");
  });
});

describe("familyName / familyLabels", () => {
  it("drops a bare repository's .git suffix but never empties the name", () => {
    expect(familyName("/srv/git/api.git")).toBe("api");
    expect(familyName("/srv/git/.git")).toBe(".git");
    expect(familyName("/Users/me/GitPulse")).toBe("GitPulse");
  });

  it("widens two repositories that share a name, so their headers read differently", () => {
    const labels = familyLabels(["/work/api", "/home/api", "/x/GitPulse"]);
    expect(labels.get("/work/api")).toBe("work/api");
    expect(labels.get("/home/api")).toBe("home/api");
    expect(labels.get("/x/GitPulse")).toBe("GitPulse");
    expect(new Set(labels.values()).size).toBe(3);
  });

  it("collapses duplicate roots to one entry", () => {
    expect(familyLabels(["/a/r", "/a/r"]).size).toBe(1);
  });
});

describe("checkoutName", () => {
  const identity = (path: string) => identityKey(path, folding);

  it("marks the primary checkout and keeps its label", () => {
    expect(checkoutName("/x/GitPulse", "GitPulse", "/x/GitPulse", identity)).toEqual({
      name: "GitPulse",
      primary: true,
      agent: "",
    });
    expect(checkoutName("/X/gitpulse/", "GitPulse", "/x/GitPulse", identity).primary).toBe(true);
  });

  it("calls an agent worktree by its session and names the agent separately", () => {
    expect(
      checkoutName("/x/GitPulse/.claude/worktrees/handoff-fix", "handoff-fix", "/x/GitPulse", identity),
    ).toEqual({ name: "handoff-fix", primary: false, agent: "claude" });
  });

  it("falls back to the tab label for a hand-made worktree or an empty slug", () => {
    expect(checkoutName("/x/GitPulse-feature", "GitPulse-feature", "/x/GitPulse", identity)).toEqual({
      name: "GitPulse-feature",
      primary: false,
      agent: "",
    });
    expect(checkoutName("/x/GitPulse/.codex/worktrees", "worktrees", "/x/GitPulse", identity).name).toBe(
      "worktrees",
    );
  });
});
