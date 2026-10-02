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

  it("stacks loaded notices instead of listing every row", () => {
    const style = source.slice(source.indexOf("<style>"));
    expect(source).toContain("notificationPile");
    expect(source).toContain("items.slice(0, pile.shown)");
    expect(source).toContain('data-testid="notification-pile"');
    expect(source).toContain('aria-expanded={expanded}');
    expect(source).toContain('aria-hidden="true"');
    expect(style).toContain(".peek{position:absolute");
    expect(style).not.toContain("border-top");
    expect(style).not.toMatch(/background(?:-color)?:\s*rgb\(var\(--c-(?:bg|surface|surface-hover)\)\)/);
  });
});
