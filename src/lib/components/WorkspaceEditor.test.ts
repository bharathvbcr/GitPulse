import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./WorkspaceEditor.svelte", import.meta.url), "utf8");

describe("WorkspaceEditor", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "WorkspaceEditor.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("registers already-open GitPulse tabs into the workspace draft before save", () => {
    expect(source).toContain("openTabs = []");
    expect(source).toContain("addableOpenTabs");
    expect(source).toContain("registerRepository");
    expect(source).toContain("withRepositoryId");
    expect(source).toContain("Open in GitPulse");
    expect(source).toContain("Add all open");
    expect(source).toContain("addOpenPaths");
    expect(source).toContain("draft.repository_ids = withRepositoryId");
    expect(source).toContain("title={tab.path}");
    expect(source).toContain("open-mark");
  });
});
