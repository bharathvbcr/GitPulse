import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

describe("TaskEditor", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskEditor.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("shows Manvi notes, title, description, due date, status, and repos immediately; folds locks and notifications", () => {
    const markup = source.slice(source.indexOf("<aside"));
    const detailsAt = markup.indexOf("More details");
    expect(detailsAt).toBeGreaterThan(0);
    const before = markup.slice(0, detailsAt);
    const inside = markup.slice(detailsAt);
    expect(before).toContain("TaskManviAssist");
    expect(before).toContain("prepareForManvi");
    expect(before).toContain("Copy for agent");
    expect(before).toContain("copyForAgent");
    expect(before).toContain("copyable");
    expect(before).toContain("Copied");
    expect(source).toContain("position:sticky");
    expect(before).toContain("bind:title={draft.title}");
    expect(before).toContain("bind:description={draft.description}");
    expect(before).toContain("bind:value={draft.status}");
    expect(before).toContain("Schedule and labels");
    expect(before).toContain("datetime-local");
    expect(before).toContain("dueInputValue");
    expect(before).toContain("membership(");
    expect(before).toContain("Open in GitPulse");
    expect(before).toContain("addOpenPaths");
    expect(inside).toContain("setFieldLock");
    expect(inside).toContain("NativeNotificationSettings");
    expect(before).not.toContain("NativeNotificationSettings");
    expect(before).not.toContain("setFieldLock");
    expect(source).toContain("SettingToggle");
    expect(source).toContain("seed");
    expect(source).toContain('class="sheet-body"');
    expect(source).toContain("consumeNotes");
    expect(source).toContain("applyExtractedNotes");
    expect(source).not.toMatch(/form\{[^}]*flex:1/);
    expect(source).not.toContain("<details");
    expect(source).not.toContain("onclick={copy}");
  });

  it("deletes through the in-app confirm and retries the same delete identity", () => {
    expect(source).toContain("askConfirm");
    expect(source).toContain("deleteTask");
    expect(source).toContain("pendingDelete");
    expect(source).toContain("Retry delete");
    expect(source).toContain("gp-btn-danger");
    expect(source).toContain("gp-glass");
    expect(source).not.toContain("window.confirm");
    expect(source).not.toContain("items.delete");
  });
});
