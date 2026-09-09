import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

describe("TaskEditor", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskEditor.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("shows title, description, and status immediately; folds locks, notifications, repos, and copy-brief", () => {
    const markup = source.slice(source.indexOf("<aside"));
    const detailsAt = markup.indexOf("<details");
    expect(detailsAt).toBeGreaterThan(0);
    const before = markup.slice(0, detailsAt);
    const inside = markup.slice(detailsAt);
    expect(before).toContain("bind:value={draft.title}");
    expect(before).toContain("bind:value={draft.description}");
    expect(before).toContain("bind:value={draft.status}");
    expect(inside).toContain("setFieldLock");
    expect(inside).toContain("NativeNotificationSettings");
    expect(inside).toContain("membership(");
    expect(inside).toContain("Open in GitPulse");
    expect(inside).toContain("addOpenPaths");
    expect(inside).toContain("onclick={copy}");
    expect(before).not.toContain("onclick={copy}");
    expect(before).not.toContain("NativeNotificationSettings");
    expect(before).not.toContain("setFieldLock");
  });
});
