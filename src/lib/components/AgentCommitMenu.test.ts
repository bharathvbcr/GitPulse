import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import AgentCommitMenu from "./AgentCommitMenu.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "AgentCommitMenu.svelte"),
  "utf8",
);

describe("AgentCommitMenu", () => {
  it("renders regular button by default", () => {
    const { body } = render(AgentCommitMenu, { props: { compact: false } });
    expect(body).toContain("Ask Agent to Commit");
    expect(body).toContain('aria-haspopup="dialog"');
    expect(body).toContain('data-agent-commit-menu');
  });

  it("renders compact button when compact is true", () => {
    const { body } = render(AgentCommitMenu, { props: { compact: true } });
    expect(body).toContain("Ask Agent");
    expect(body).toContain('rounded-full');
    expect(body).toContain('data-agent-commit-menu');
  });

  it("includes Antigravity and Claude Code launch options in source", () => {
    expect(source).toContain("handleLaunch");
    expect(source).toContain('"agy"');
    expect(source).toContain('"claude"');
    expect(source).toContain("Ask Antigravity (agy)");
    expect(source).toContain("Ask Claude Code");
  });

  it("provides Developer CLI and Python SDK inspection and copying", () => {
    expect(source).toContain("getAgentCommitCliCommand");
    expect(source).toContain("getAgentCommitSdkSnippet");
    expect(source).toContain("CLI & Developer SDK");
    expect(source).toContain("handleCopy");
    expect(source).toContain("copyText");
  });

  /**
   * The panel used to be an `absolute` child of its trigger, so every
   * clipping ancestor — the sidebar scroller, the 48px collapsed rail — cut
   * it off. It is now portaled out and anchored by the popover owner; these
   * pin the shape the browser harness (`harness/uncommitted.html`) measures.
   */
  it("portals the panel out of its clipping pane before placing it", () => {
    const panel = source.slice(source.indexOf("{#if open}"));
    const portalAt = panel.indexOf('use:portal={"body"}');
    const popoverAt = panel.indexOf("use:popover={dismissal}");
    expect(portalAt).toBeGreaterThan(-1);
    // Order matters: popover measures the node, so it must already be in <body>.
    expect(popoverAt).toBeGreaterThan(portalAt);
    expect(panel).toMatch(/class="fixed /);
    expect(panel).not.toMatch(/\b(top-full|bottom-full)\b/);
    // The panel is a sibling of the trigger wrapper, never inside it.
    const wrapperEnd = source.indexOf("</div>\n\n{#if open}");
    expect(wrapperEnd).toBeGreaterThan(-1);
  });

  it("anchors to its trigger and keeps the panel inside the window", () => {
    expect(source).toContain('kind: "element"');
    expect(source).toContain("element: triggerEl");
    expect(source).toContain('place: direction === "up" ? "above" : "below"');
    expect(source).toContain('align: align === "right" ? "end" : "start"');
    expect(source).toContain("max-h-[calc(100vh-16px)]");
    expect(source).toContain("LAYERS.MENU");
  });

  it("scopes dismissal to this instance, not to every mounted menu", () => {
    // A bare `[data-agent-commit-menu]` matched all four mounts' triggers, so
    // opening a second menu counted as a click inside the first.
    expect(source).not.toContain('inside: "[data-agent-commit-menu]"');
    expect(source).toContain('[data-agent-commit-menu="${uid}"], [data-agent-commit-panel="${uid}"]');
    expect(source).toContain("$props.id()");
  });

  it("never launches a prompt-builder failure as if it were the prompt", () => {
    // The old derived prompt returned formatError(err) as the prompt text.
    expect(source).not.toMatch(/return formatError\(err\);/);
    expect(source).toContain("const blockedReason = $derived(disabledReason ?? prompt.error);");
  });

  it("handles staging scope toggle and custom note", () => {
    expect(source).toContain("scope === \"staged\"");
    expect(source).toContain("userNote");
    expect(source).toContain("agent-commit-note");
  });

  it("fails closed on merge conflicts and clean working trees", () => {
    expect(source).toContain("AGENT_COMMIT_CONFLICTS");
    expect(source).toContain("AGENT_COMMIT_CLEAN");
    expect(source).toContain("AGENT_COMMIT_NO_REPO");
    expect(source).toContain("conflictedCount > 0");
    expect(source).toContain("disabledReason");
  });
});
