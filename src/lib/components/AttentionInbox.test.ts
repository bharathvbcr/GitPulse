import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./AttentionInbox.svelte", import.meta.url), "utf8");

describe("AttentionInbox", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "AttentionInbox.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("keeps open rows to title, relative time, and one primary action", () => {
    expect(source).not.toContain("Activity inbox");
    expect(source).not.toContain("Agent requests, run outcomes");
    expect(source).not.toContain("Task {item.task_id}");
    expect(source).toContain("formatRelativeTime");
    expect(source).toContain(">Open<");
    expect(source).not.toContain("Mark read");
    expect(source).not.toContain("Snooze 1h");
  });
});
