import { describe, expect, it } from "vitest";
import {
  addableOpenTabs,
  identityCommonDir,
  membershipAfterAttach,
  openAddActionLabel,
  openMembershipCandidates,
  pickerSelectionIds,
  registeredIdForPath,
  tabMatchesRegistered,
  withRepositoryId,
} from "./openMembership";

const ci = { caseInsensitive: true };
const cs = { caseInsensitive: false };

const gitpulse = { id: "r1", identity_key: "local:/Users/me/Code/GitPulse/.git" };
const manvi = { id: "r2", identity_key: "local:/Users/me/Code/Manvi/.git" };

describe("identity matching", () => {
  it("reads the git common dir from a local identity key", () => {
    expect(identityCommonDir("local:/tmp/repo/.git")).toBe("/tmp/repo/.git");
    expect(identityCommonDir("clone:abc")).toBeNull();
    expect(identityCommonDir("local:")).toBeNull();
  });

  it("matches a worktree path to local:{path}/.git and to local:{path}", () => {
    expect(tabMatchesRegistered("/tmp/repo", "local:/tmp/repo/.git", cs)).toBe(true);
    expect(tabMatchesRegistered("/tmp/repo/", "local:/tmp/repo", cs)).toBe(true);
    expect(tabMatchesRegistered("/tmp/other", "local:/tmp/repo/.git", cs)).toBe(false);
  });

  it("treats path case as the filesystem does", () => {
    expect(tabMatchesRegistered("/Tmp/Repo", "local:/tmp/repo/.git", ci)).toBe(true);
    expect(tabMatchesRegistered("/Tmp/Repo", "local:/tmp/repo/.git", cs)).toBe(false);
  });

  it("does not treat a path prefix as the same repository", () => {
    expect(tabMatchesRegistered("/tmp/repo-extra", "local:/tmp/repo/.git", cs)).toBe(false);
    expect(tabMatchesRegistered("/tmp/repo", "local:/tmp/repo-extra/.git", cs)).toBe(false);
  });
});

describe("open membership candidates", () => {
  it("dedupes open tabs, maps registered identities, and hides members", () => {
    const tabs = [
      { path: "/Users/me/Code/GitPulse", label: "GitPulse" },
      { path: "/Users/me/Code/GitPulse/", label: "GitPulse" },
      { path: "/Users/me/Code/Manvi", label: "Manvi" },
      { path: "/Users/me/Code/Other", label: "Other" },
    ];
    const candidates = openMembershipCandidates(tabs, [gitpulse, manvi], ["r1"], ci);
    expect(candidates).toEqual([
      { path: "/Users/me/Code/GitPulse", label: "GitPulse", registeredId: "r1", alreadyMember: true },
      { path: "/Users/me/Code/Manvi", label: "Manvi", registeredId: "r2", alreadyMember: false },
      { path: "/Users/me/Code/Other", label: "Other", registeredId: null, alreadyMember: false },
    ]);
    expect(addableOpenTabs(candidates).map((tab) => tab.label)).toEqual(["Manvi", "Other"]);
  });

  it("skips empty or control-character paths rather than inventing a row", () => {
    expect(openMembershipCandidates([{ path: "  ", label: "blank" }], [], [], cs)).toEqual([]);
    expect(registeredIdForPath("/missing", [gitpulse], cs)).toBeNull();
  });
});

describe("membership attach", () => {
  it("appends new ids without duplicating or reordering existing ones", () => {
    expect(withRepositoryId(["a", "b"], "b")).toEqual(["a", "b"]);
    expect(withRepositoryId(["a", "b"], "c")).toEqual(["a", "b", "c"]);
    expect(membershipAfterAttach(["a"], ["a", "b", "a", "c"])).toEqual(["a", "b", "c"]);
    expect(membershipAfterAttach(["a", "b"], ["a"])).toEqual(["a", "b"]);
  });
});

describe("picker copy", () => {
  it("selects workspace members when known, otherwise the catalog", () => {
    expect(pickerSelectionIds("workspace", ["w1"], ["c1", "c2"])).toEqual(["w1"]);
    expect(pickerSelectionIds("workspace", null, ["c1"])).toEqual([]);
    expect(pickerSelectionIds("global", ["w1"], ["c1", "c2"])).toEqual(["c1", "c2"]);
    expect(pickerSelectionIds("repository", null, ["c1"])).toEqual(["c1"]);
  });

  it("names a one-click add from the open-tab set", () => {
    expect(openAddActionLabel([])).toBeNull();
    expect(openAddActionLabel([{ label: "GitPulse" }])).toBe("Add GitPulse");
    expect(openAddActionLabel([{ label: "A" }, { label: "B" }])).toBe("Add 2 open repositories");
  });
});
