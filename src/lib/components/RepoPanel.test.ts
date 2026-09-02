import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import RepoPanel from "./RepoPanel.svelte";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "RepoPanel.svelte"), "utf8");

describe("RepoPanel remotes", () => {
  it("renders the remotes heading", () => {
    const { body } = render(RepoPanel);
    expect(body).toContain("Remotes");
  });

  it("invokes add, set-url, rename, prune and remove through the store", () => {
    expect(source).toContain('kind: "add"');
    expect(source).toContain('kind: "seturl"');
    expect(source).toContain('kind: "rename"');
    expect(source).toContain('kind: "prune"');
    expect(source).toContain('kind: "remove"');
    expect(source).toContain("repoStore.remoteChange");
    expect(source).toContain("isDestructiveRemoteChange");
    expect(source).toContain("remoteChangeConsequence");
    expect(source).toContain("submitRename");
  });

  it("says when the remote list was capped instead of looking complete", () => {
    expect(source).toContain("remotesTruncated");
    expect(source).toContain("More exist in .git/config and are not listed.");
  });

  it("arms destructive remote changes before executing them", () => {
    expect(source).toContain("Confirm prune");
    expect(source).toContain("Confirm remove");
    expect(source).toContain("Confirm URL");
  });
});

describe("RepoPanel submodules", () => {
  it("exposes sync and deinit, not only initialize", () => {
    expect(source).toContain('kind: "sync"');
    expect(source).toContain('kind: "deinit"');
    expect(source).toContain("activateSubmodule");
    expect(source).toContain("canDeinit");
    expect(source).toContain("canSync");
  });

  it("says when the submodule list was capped instead of looking complete", () => {
    expect(source).toContain("submodulesTruncated");
    expect(source).toContain("More exist and are not listed.");
  });

  it("never force-deinits; an unknown dirty tree is not discarded", () => {
    expect(source).toContain("force: false");
    expect(source).not.toMatch(/kind:\s*"deinit"[\s\S]{0,80}force:\s*true/);
    expect(source).toContain("isDestructiveSubmoduleChange");
    expect(source).toContain("Confirm deinit");
  });
});
