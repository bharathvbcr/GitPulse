import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import DockAgentsSection, { type DockAgentRow } from "./DockAgentsSection.svelte";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "DockAgentsSection.svelte"), "utf8");

const launchers = [
  { kind: "claude" as const, label: "Claude", disabled: false, title: "Start Claude" },
  { kind: "codex" as const, label: "Codex", disabled: true, title: "Codex is not installed" },
];
const hereRow: DockAgentRow = {
  id: "tab-1",
  launcher: "claude",
  title: "Fix login",
  detail: "main",
  status: { label: "Working", cls: "text-accent" },
  attention: "Waiting",
  active: true,
  unread: true,
};
const elseRow: DockAgentRow = { id: "reg-9", launcher: "codex", title: "Docs pass", detail: "other/wt", attention: "Failed" };

function html(over: Record<string, unknown> = {}) {
  return render(DockAgentsSection, {
    props: {
      here: [hereRow],
      elsewhere: [elseRow],
      launchers,
      followAgent: false,
      canFollow: true,
      onSelect: vi.fn(),
      onGoTo: vi.fn(),
      onLaunch: vi.fn(),
      onToggleFollow: vi.fn(),
      ...over,
    },
  }).body;
}

describe("DockAgentsSection", () => {
  it("renders here and elsewhere rows with status, unread and detail", () => {
    const body = html();
    expect(body).toContain('data-agent-id="tab-1"');
    expect(body).toContain('data-agent-id="reg-9"');
    expect(body).toContain("In this repository");
    expect(body).toContain("Other repositories");
    expect(body).toContain("Working");
    expect(body).toContain("Unread output");
    expect(body).toContain("other/wt");
    expect(body).toContain('aria-current="true"');
  });

  it("wires here rows to onSelect and elsewhere rows to onGoTo", () => {
    expect(source).toContain("{@render rowList(here, onSelect)}");
    expect(source).toContain("{@render rowList(elsewhere, onGoTo)}");
    expect(source).toContain("onclick={() => onPick(row.id)}");
  });

  it("wires launchers to onLaunch and renders disabled ones disabled", () => {
    const body = html();
    expect(body).toContain('data-testid="dock-agents-launch-claude"');
    expect(body).toMatch(/<button[^>]*data-testid="dock-agents-launch-codex"[^>]*disabled/);
    expect(body).not.toMatch(/<button[^>]*data-testid="dock-agents-launch-claude"[^>]*disabled/);
    expect(source).toContain("onclick={() => onLaunch(launcher.kind)}");
  });

  it("shows the empty state only when nothing runs anywhere", () => {
    const body = html({ here: [], elsewhere: [] });
    expect(body).toContain('data-testid="dock-agents-empty"');
    expect(body).toContain("No agents running");
    expect(html()).not.toContain("dock-agents-empty");
    const onlyElsewhere = html({ here: [] });
    expect(onlyElsewhere).toContain("None in this repository");
    expect(onlyElsewhere).not.toContain("dock-agents-empty");
  });

  it("draws Failed as danger and other attention as warn", () => {
    const body = html();
    expect(body).toMatch(/data-tone="danger"[^>]*>Failed/);
    expect(body).toMatch(/data-tone="warn"[^>]*>Waiting/);
  });

  it("reflects follow state in aria-pressed and disables it when it cannot follow", () => {
    expect(html({ followAgent: true })).toMatch(/data-testid="dock-agents-follow"[^>]*aria-pressed="true"|aria-pressed="true"[^>]*data-testid="dock-agents-follow"/);
    expect(html({ followAgent: false })).toMatch(/aria-pressed="false"/);
    expect(html({ canFollow: false })).toMatch(/<button[^>]*data-testid="dock-agents-follow"[^>]*disabled/);
  });
});
