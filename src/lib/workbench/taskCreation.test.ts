import { describe, expect, it } from "vitest";
import type { Scope } from "./client";
import { quickAddRefusal, taskCreation } from "./taskCreation";

const repos = [{ id: "repo-0" }, { id: "repo-1" }];
const global: Scope = { kind: "global" };
const workspace: Scope = { kind: "workspace", id: "ws-1" };
const repository: Scope = { kind: "repository", id: "repo-1" };

const ready = { initialized: true, repositories: repos, workspaceMembers: null };

describe("taskCreation", () => {
  it("refuses before the catalog has loaded, and says it is loading rather than blaming the reader", () => {
    const creation = taskCreation(global, { ...ready, initialized: false });
    expect(creation.allowed).toBe(false);
    expect(creation.blocked).toBe("Tasks are still loading.");
    expect(creation.caveat).toBeNull();
  });

  it("refuses a profile with no repositories, with the remedy in the message", () => {
    const creation = taskCreation(global, { ...ready, repositories: [] });
    expect(creation.allowed).toBe(false);
    expect(creation.blocked).toBe("Add a repository to create tasks.");
    expect(creation.seed).toEqual({ repositoryIds: [], primaryRepositoryId: "", homeWorkspaceId: null });
  });

  it("seeds a repository scope with itself and a global scope with the first catalog entry", () => {
    expect(taskCreation(repository, ready).seed).toEqual({
      repositoryIds: ["repo-1"], primaryRepositoryId: "repo-1", homeWorkspaceId: null,
    });
    expect(taskCreation(global, ready).seed).toEqual({
      repositoryIds: ["repo-0"], primaryRepositoryId: "repo-0", homeWorkspaceId: null,
    });
  });

  it("seeds a populated workspace with its first member and keeps the workspace as home", () => {
    const creation = taskCreation(workspace, { ...ready, workspaceMembers: ["repo-1"] });
    expect(creation.allowed).toBe(true);
    expect(creation.caveat).toBeNull();
    expect(creation.seed).toEqual({
      repositoryIds: ["repo-1"], primaryRepositoryId: "repo-1", homeWorkspaceId: "ws-1",
    });
  });

  it("lets an empty workspace create a task, naming the workspace and what the sheet will ask for", () => {
    // The board used to refuse here, with nothing on screen saying why: the
    // profile has repositories, so the sheet can link one. A refusal was a
    // dead end, not a safeguard.
    const creation = taskCreation(workspace, {
      ...ready, workspaceMembers: [], workspaceName: "Developer tools",
    });
    expect(creation.allowed).toBe(true);
    expect(creation.blocked).toBeNull();
    expect(creation.caveat).toBe("Developer tools has no repositories yet. Choose one in the task.");
    expect(creation.seed).toEqual({ repositoryIds: [], primaryRepositoryId: "", homeWorkspaceId: "ws-1" });
  });

  it("falls back to a generic caveat when the workspace name has not loaded", () => {
    for (const workspaceName of [undefined, "", "   "]) {
      const creation = taskCreation(workspace, { ...ready, workspaceMembers: [], workspaceName });
      expect(creation.caveat).toBe("This workspace has no repositories yet. Choose one in the task.");
    }
  });

  it("distinguishes an unread membership from an empty one, and still allows the task", () => {
    // `null` is "not asked yet, or the read failed". Reporting it as an empty
    // workspace would name a cause the board does not know.
    const creation = taskCreation(workspace, { ...ready, workspaceMembers: null, workspaceName: "Developer tools" });
    expect(creation.allowed).toBe(true);
    expect(creation.caveat).toBe("This workspace's repositories could not be read. Choose one in the task.");
  });

  it("never reports both a refusal and a caveat", () => {
    const contexts = [
      { ...ready, initialized: false },
      { ...ready, repositories: [] },
      { ...ready, workspaceMembers: [] },
      { ...ready, workspaceMembers: ["repo-0"] },
    ];
    for (const scope of [global, workspace, repository]) {
      for (const context of contexts) {
        const creation = taskCreation(scope, context);
        expect(creation.allowed).toBe(creation.blocked === null);
        if (creation.blocked) expect(creation.caveat).toBeNull();
      }
    }
  });
});

describe("quickAddRefusal", () => {
  it("repeats the board's own refusal when creation is blocked outright", () => {
    expect(quickAddRefusal(taskCreation(global, { ...ready, repositories: [] })))
      .toBe("Add a repository to create tasks.");
  });

  it("names the marker and the editor when the scope seeds no repository", () => {
    // Quick add cannot open a picker, so the line has to carry the repository.
    const refusal = quickAddRefusal(taskCreation(workspace, { ...ready, workspaceMembers: [] }));
    expect(refusal).toContain("^name");
    expect(refusal).toContain("Shift+Return");
  });

  it("blames the missing title when the scope could have saved the line", () => {
    expect(quickAddRefusal(taskCreation(repository, ready))).toBe("Add a title before this line can be saved.");
  });
});
