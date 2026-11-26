import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import AttentionInbox from "./AttentionInbox.svelte";

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

  it("lets every row be dismissed, and a dismissed row restored", () => {
    expect(source).toContain('aria-label="Dismiss notification"');
    expect(source).toContain('setDismissed(item, "dismiss")');
    expect(source).toContain('setDismissed(item, "restore")');
    // Both row shapes carry the control: a notice this build cannot read is still one the reader can clear.
    expect(source.match(/\{@render dismissControl\(item\)\}/g)).toHaveLength(2);
    expect(source).toContain("withAttentionUpdate(result, updated, filter)");
    expect(source).toContain("onchange?.()");
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

  it("renders the Inbox header alongside the current Waylode badge Logo inside aria-hidden", () => {
    const { body } = render(AttentionInbox, {
      props: { scope: { kind: "global" }, onopen: async () => {} },
    });
    expect(body).toContain("<strong>Inbox</strong>");
    expect(body).toContain("M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8");
    expect(body).toMatch(/id="wl-[a-z0-9]+-badge"/);
    expect(body).toMatch(/<span[^>]*aria-hidden="true"[^>]*>[\s\S]*?<svg[^>]*aria-label="Waylode"/);
  });
});
