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
  it("renders attention tray above repository groups in default auto mode with status, unread and detail", () => {
    const body = html();
    expect(body).toContain('data-agent-id="tab-1"');
    expect(body).toContain('data-agent-id="reg-9"');
    expect(body).toContain('data-testid="dock-agents-attention-tray"');
    expect(body).toContain('data-testid="dock-agents-repo-group"');
    expect(body.indexOf('data-testid="dock-agents-attention-tray"')).toBeLessThan(
      body.indexOf('data-testid="dock-agents-repo-group"'),
    );
    expect(body).toContain("Needs attention");
    expect(body).toContain("Working");
    expect(body).toContain("Unread output");
    expect(body).toContain("other/wt");
    expect(body).toContain('aria-current="true"');
  });

  it("renders flat triage sections when initialGroupMode is flat", () => {
    const body = html({ initialGroupMode: "flat" });
    expect(body).not.toContain('data-testid="dock-agents-attention-tray"');
    expect(body).toContain("In this repository");
    expect(body).toContain("Other repositories");
  });

  it("wires here rows to onSelect and elsewhere rows to onGoTo", () => {
    expect(source).toContain("{@render rowList(triageHere, onSelect, true)}");
    expect(source).toContain("{@render rowList(triageElsewhere, onGoTo, false, true)}");
    expect(source).toContain("item.isHere ? onSelect : onGoTo");
    expect(source).toContain("item.isHere ? onSelect(row.id) : onGoTo(row.id)");
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
    const onlyElsewhere = html({ here: [], initialGroupMode: "flat" });
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

  it("always shows a status slot: Idle fallback only when status is absent", () => {
    expect(html()).toContain(">Idle<");
    const withStatus = html({ elsewhere: [] });
    expect(withStatus).toContain(">Working<");
    expect(withStatus).not.toContain(">Idle<");
  });

  it("gives rows an action title", () => {
    const body = html();
    expect(body).toContain("Show in Shell \u2014 Fix login");
    expect(body).toContain("Go to Docs pass in other/wt");
    expect(html({ elsewhere: [{ ...elseRow, detail: "" }] })).toContain('title="Go to Docs pass"');
  });

  it("adds accent classes to Follow only when pressed", () => {
    const on = html({ followAgent: true });
    const off = html({ followAgent: false });
    expect(on).toMatch(/<button[^>]*data-testid="dock-agents-follow"[^>]*/);
    expect(on.match(/<button[^>]*data-testid="dock-agents-follow"[^>]*>/)![0]).toContain("text-accent bg-accent/15");
    expect(off.match(/<button[^>]*data-testid="dock-agents-follow"[^>]*>/)![0]).not.toContain("bg-accent/15");
    expect(on).toContain("Follow edits");
  });

  it("draws a hover/focus arrow that is aria-hidden", () => {
    expect(source).toContain("group-hover:opacity-100 group-focus-visible:opacity-100");
    const body = html();
    expect(body).toMatch(/<svg[^>]*opacity-0[^>]*aria-hidden="true"|<svg[^>]*aria-hidden="true"[^>]*opacity-0/);
  });

  it("renders heading counts as count chips", () => {
    expect(html()).toContain('data-kind="count"');
  });

  it("says in view why no agent can start when every launcher is disabled", () => {
    const full = "All 32 terminal sessions are open — close one under ⋯ → All sessions, or raise the limit in Settings → Agents";
    const blocked = html({ launchers: launchers.map((launcher) => ({ ...launcher, disabled: true, title: full })) });
    expect(blocked).toContain('data-testid="dock-agents-blocked"');
    expect(blocked).toContain('role="status"');
    expect(blocked).toContain(full);
    // One launcher still able to start: no blocked notice, the tooltips carry the rest.
    expect(html()).not.toContain("dock-agents-blocked");
  });

  it("explains a disabled Follow edits", () => {
    expect(html({ canFollow: false })).toContain('title="Open a repository to follow agent edits"');
  });

  it("auto-groups by repo when no agent needs attention or is done, hiding the tray and showing repo logos and running chips", () => {
    const runningHere: DockAgentRow = {
      id: "tab-run",
      launcher: "claude",
      title: "Build search",
      detail: "main",
      repoPath: "/work/gitpulse",
      repoName: "GitPulse",
      checkout: "main",
      status: { label: "Running", cls: "text-accent" },
      running: true,
    };
    const idleElsewhere: DockAgentRow = {
      id: "reg-idle",
      launcher: "codex",
      title: "Review PR",
      detail: "Alpha · feat/ui",
      repoPath: "/work/alpha",
      repoName: "Alpha",
      checkout: "feat/ui",
      status: { label: "Idle", cls: "text-textMuted" },
      running: false,
    };
    const body = html({
      here: [runningHere],
      elsewhere: [idleElsewhere],
      currentRepoPath: "/work/gitpulse",
      currentRepoName: "GitPulse",
    });
    expect(body).not.toContain('data-testid="dock-agents-attention-tray"');
    expect(body).toContain('data-testid="dock-agents-repo-group"');
    expect(body).toContain("This repository");
    expect(body).toContain("Alpha");
    expect(body).toContain("1 running");
    expect(body).toContain("data-repo-logo");
    expect(body).toContain('data-testid="dock-agent-running"');
    expect(body).toContain('data-testid="dock-agent-live-dot"');
  });

  it("hides the attention tray and renders ungrouped rows when filtered to attention or done", () => {
    const doneHere: DockAgentRow = {
      id: "tab-done",
      launcher: "claude",
      title: "Patch tests",
      detail: "main",
      repoPath: "/work/gitpulse",
      repoName: "GitPulse",
      status: { label: "Done", cls: "text-ok" },
      done: true,
    };
    const attElse: DockAgentRow = {
      id: "reg-att",
      launcher: "codex",
      title: "Migrate schema",
      detail: "Beta · db",
      repoPath: "/work/beta",
      repoName: "Beta",
      attention: "Needs input",
      attentionDetail: "Approve ALTER TABLE migration",
    };
    const body = html({
      here: [doneHere],
      elsewhere: [attElse],
      currentRepoPath: "/work/gitpulse",
      currentRepoName: "GitPulse",
      initialFilter: "attention",
    });
    expect(body).not.toContain('data-testid="dock-agents-attention-tray"');
    expect(body).not.toContain('data-testid="dock-agents-repo-group"');
    expect(body).toContain("Other repositories");
    expect(body).toContain("Migrate schema");
    expect(body).toContain("data-repo-logo");
  });

  it("renders attention tray with repo logos, done checkmarks, attentionDetail, search bar, filters, and Open Agents", () => {
    const doneHere: DockAgentRow = {
      id: "tab-done",
      launcher: "claude",
      title: "Patch tests",
      detail: "main",
      repoPath: "/work/gitpulse",
      repoName: "GitPulse",
      status: { label: "Done", cls: "text-ok" },
      done: true,
    };
    const attElse: DockAgentRow = {
      id: "reg-att",
      launcher: "codex",
      title: "Migrate schema",
      detail: "Beta · db",
      repoPath: "/work/beta",
      repoName: "Beta",
      attention: "Needs input",
      attentionDetail: "Approve ALTER TABLE migration",
    };
    const body = html({
      here: [doneHere],
      elsewhere: [attElse],
      currentRepoPath: "/work/gitpulse",
      currentRepoName: "GitPulse",
      onOpenGlobalAgents: () => {},
    });
    expect(body).toContain('data-testid="dock-agents-attention-tray"');
    expect(body).toContain("Needs attention &amp; finished");
    expect(body).toContain('data-testid="dock-agents-attention-item"');
    expect(body).toContain('data-testid="dock-agents-search"');
    expect(body).toContain('data-testid="dock-agents-filter-all"');
    expect(body).toContain('data-testid="dock-agents-filter-running"');
    expect(body).toContain('data-testid="dock-agents-filter-attention"');
    expect(body).toContain('data-testid="dock-agents-group-toggle"');
    expect(body).toContain('data-testid="dock-agents-open-global"');
    expect(body).toContain('data-testid="dock-agent-done"');
    expect(body).toContain("Approve ALTER TABLE migration");
    expect(body).toContain("data-repo-logo");
    expect(body).toContain('data-testid="dock-agents-filter-done"');
    expect(body).toContain('data-testid="dock-agents-pinned-header"');
    expect(body).toContain('data-testid="dock-agents-list"');
    expect(body).toContain('data-testid="dock-agents-search-hint"');
  });

  it("renders active badge, recency timestamp, and omits redundant repoName in grouped rows", () => {
    const activeHere: DockAgentRow = {
      id: "tab-act",
      launcher: "claude",
      title: "Fix auth",
      detail: "GitPulse",
      repoPath: "/work/gitpulse",
      repoName: "GitPulse",
      checkout: null,
      active: true,
      running: true,
      status: { label: "Running", cls: "text-accent" },
      lastActivityAt: Date.now() - 120_000,
    };
    const elsePrimary: DockAgentRow = {
      id: "reg-prim",
      launcher: "codex",
      title: "Lint code",
      detail: "Alpha",
      repoPath: "/work/alpha",
      repoName: "Alpha",
      checkout: null,
      running: false,
      status: { label: "Idle", cls: "text-textMuted" },
    };
    const body = html({
      here: [activeHere],
      elsewhere: [elsePrimary],
      currentRepoPath: "/work/gitpulse",
      currentRepoName: "GitPulse",
    });
    expect(body).toContain('data-testid="dock-agent-active"');
    expect(body).toContain('data-testid="dock-agent-age"');
    expect(body).toContain("2m ago");
    // Grouped rows should not repeat "Alpha" as a subtitle underneath the Alpha header.
    expect(body.match(/>Alpha</g)).toHaveLength(1);
  });

  it("sorts attention and done rows ahead of running and idle rows in flat triage mode", () => {
    const idleRow: DockAgentRow = { id: "t-idle", launcher: "claude", title: "Idle agent", status: { label: "Idle", cls: "text-textMuted" } };
    const runRow: DockAgentRow = { id: "t-run", launcher: "claude", title: "Run agent", status: { label: "Running", cls: "text-accent" }, running: true };
    const doneRow: DockAgentRow = { id: "t-done", launcher: "claude", title: "Done agent", status: { label: "Done", cls: "text-ok" }, done: true };
    const failRow: DockAgentRow = { id: "t-fail", launcher: "claude", title: "Fail agent", attention: "Failed" };
    const body = html({ here: [idleRow, runRow, doneRow, failRow], elsewhere: [], initialGroupMode: "flat" });
    const order = ["t-fail", "t-done", "t-run", "t-idle"].map((id) => body.indexOf(`data-agent-id="${id}"`));
    expect(order.every((i) => i > -1)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });
});
