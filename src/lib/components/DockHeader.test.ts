import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import DockHeader from "./DockHeader.svelte";
import { dockStatus } from "../terminal/dockSections";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "DockHeader.svelte"), "utf8");

const base = {
  section: "shell" as const,
  onSection: () => {},
  os: "linux" as const,
  repoName: "gitpulse",
  repoPath: "/work/gitpulse",
  checkoutLabel: "feature-x",
  branch: "main",
  sessionCountLabel: "3 open / 32 limit",
  onToggleSessionList: () => {},
};

function html(props: Record<string, unknown> = {}): string {
  return render(DockHeader, { props: { ...base, ...props } }).body;
}

/** The opening tag of the first element carrying `needle`. */
function tagOf(body: string, needle: string): string {
  const at = body.indexOf(needle);
  expect(at, needle).toBeGreaterThan(-1);
  return body.slice(body.lastIndexOf("<", at), body.indexOf(">", at) + 1);
}

describe("DockHeader section switcher", () => {
  it("is a pressed-button group with one button per section, never a tablist", () => {
    const body = html();
    expect(body).toContain('role="group"');
    expect(body).toContain('aria-label="Dock section"');
    expect(body).not.toContain('role="tablist"');
    for (const id of ["shell", "agents", "files", "console"]) {
      expect(body).toContain(`data-testid="dock-section-${id}"`);
    }
  });

  it("marks only the current section pressed", () => {
    for (const current of ["shell", "agents", "files", "console"] as const) {
      const body = html({ section: current });
      for (const id of ["shell", "agents", "files", "console"]) {
        const tag = tagOf(body, `data-testid="dock-section-${id}"`);
        expect(tag).toContain(`aria-pressed="${id === current}"`);
        expect(tag).toContain(`data-active="${id === current}"`);
      }
    }
  });

  it("names the section's chord for the host in its tooltip", () => {
    expect(tagOf(html({ os: "macos" }), 'data-testid="dock-section-agents"')).toContain("⌥⇧⌘2");
    expect(tagOf(html({ os: "linux" }), 'data-testid="dock-section-files"')).toContain("Ctrl+Alt+Shift+3");
  });

  it("names each section button by its label text and gives its chord as aria-keyshortcuts", () => {
    const mac = html({ os: "macos" });
    const other = html({ os: "linux" });
    ["Shell", "Agents", "Files", "Console"].forEach((label, i) => {
      const id = label.toLowerCase();
      // Named by its (visually collapsible) label text, never an aria-label,
      // which would drop the Agents button's screen-reader attention count.
      expect(tagOf(mac, `data-testid="dock-section-${id}"`)).not.toContain("aria-label=");
      expect(mac).toContain(`<span class="@max-[34rem]:sr-only">${label}</span>`);
      expect(tagOf(mac, `data-testid="dock-section-${id}"`)).toContain(`aria-keyshortcuts="Alt+Shift+Meta+${i + 1}"`);
      expect(tagOf(other, `data-testid="dock-section-${id}"`)).toContain(`aria-keyshortcuts="Control+Alt+Shift+${i + 1}"`);
    });
  });

  it("collapses to icons in a narrow dock via a container query", () => {
    const body = html();
    expect(tagOf(body, 'data-testid="dock-header"')).toContain("@container");
    expect(body).toContain("@max-[34rem]:sr-only");
    expect(tagOf(body, 'data-testid="terminal-checkout-pill"')).toContain("@max-[34rem]:hidden");
  });

  it("calls back with the chosen section", () => {
    // Wiring is asserted on the source: the server renderer drops handlers.
    expect(source).toContain("onclick={() => onSection(entry.id)}");
  });

  it("shows the attention dot and a spoken count only when an agent needs the reader", () => {
    expect(html()).not.toContain("dock-agents-attention");
    const one = html({ agentAttention: 1 });
    expect(one).toContain("dock-agents-attention");
    expect(one).toContain("1 agent needs you");
    expect(html({ agentAttention: 3 })).toContain("3 agents need you");
  });
});

describe("DockHeader context and status", () => {
  it("merges repository, checkout and branch into one chip", () => {
    const body = html();
    const chip = tagOf(body, 'data-testid="terminal-checkout-pill"');
    expect(chip).toContain('title="/work/gitpulse"');
    expect(body).toContain("gitpulse · feature-x · main");
    // A label, not a control: no pill chrome.
    expect(chip).not.toContain("gp-chip");
    expect(html({ repoName: null, repoPath: null, checkoutLabel: null, branch: null })).toContain("No repository");
  });

  it("draws nothing for status when the checkout is clean", () => {
    const body = html({ status: dockStatus({ conflictedCount: 0, collision: null }) });
    for (const id of ["terminal-conflict-pill", "terminal-collision-pill", "terminal-collision-unverified"]) {
      expect(body).not.toContain(id);
    }
  });

  it("shows only the primary status, with the testid of its kind, and lists the rest in the tooltip", () => {
    const status = dockStatus({ conflictedCount: 2, collision: { ok: true, overlapping_files: 3, shared_worktree_files: 0 } });
    const body = html({ status, statusDetail: null });
    const chip = tagOf(body, 'data-testid="terminal-conflict-pill"');
    expect(chip).toContain('data-tone="danger"');
    expect(chip).toContain("3 cross-worktree collisions");
    expect(body).not.toContain("terminal-collision-pill");
    expect(body).toContain("2 conflicts");
  });

  it("maps collision and unverified to their own testids and tones", () => {
    const warn = html({ status: dockStatus({ conflictedCount: 0, collision: { ok: true, overlapping_files: 1, shared_worktree_files: 0 } }) });
    expect(tagOf(warn, 'data-testid="terminal-collision-pill"')).toContain('data-tone="warn"');
    const quiet = html({
      status: dockStatus({ conflictedCount: 0, collision: { ok: false, overlapping_files: 0, shared_worktree_files: 0 } }),
      statusDetail: "git unavailable",
    });
    const chip = tagOf(quiet, 'data-testid="terminal-collision-unverified"');
    expect(chip).toContain('data-tone="neutral"');
    expect(chip).toContain("git unavailable");
  });

  it("offers the way back to Shell only where the panes are off screen, and only with an active session", () => {
    const session = { launcher: "claude" as const, launcherLabel: "Claude", primary: "Fix tests", fullTitle: "Fix tests" };
    // Shell and Files both keep the active session on screen: the chip would repeat it.
    expect(html({ section: "shell", activeSession: session })).not.toContain("terminal-active-session-summary");
    expect(html({ section: "files", activeSession: session })).not.toContain("terminal-active-session-summary");
    expect(html({ section: "agents", activeSession: null })).not.toContain("terminal-active-session-summary");
    for (const section of ["agents", "console"]) {
      const body = html({ section, activeSession: session });
      expect(body).toContain('data-testid="terminal-active-session-summary"');
      expect(body).toContain("→ Shell");
    }
  });
});

describe("DockHeader return chip in Agents", () => {
  it("is dropped in Agents when the active session is already a listed row, but kept in Console", () => {
    const session = { launcher: "claude" as const, launcherLabel: "Claude", primary: "Fix tests", fullTitle: "Fix tests" };
    expect(html({ section: "agents", activeSession: session, activeSessionListed: true })).not.toContain("terminal-active-session-summary");
    expect(html({ section: "agents", activeSession: session, activeSessionListed: false })).toContain("terminal-active-session-summary");
    const consoleBody = html({ section: "console", activeSession: session, activeSessionListed: true });
    const chip = tagOf(consoleBody, 'data-testid="terminal-active-session-summary"');
    expect(chip).toContain("gp-chip");
    expect(chip).toContain('data-tone="accent"');
  });
});

describe("DockHeader session limit", () => {
  it("keeps the count in the ⋯ menu until every slot is taken, then shows it in the header", () => {
    expect(html({ sessionCountLabel: "3 open / 32 limit" })).not.toContain("dock-session-limit");
    const full = html({ sessionCountLabel: "32 open / 32 limit", sessionsAtLimit: true });
    expect(tagOf(full, 'data-testid="dock-session-limit"')).toContain('data-tone="warn"');
    expect(full).toContain("32 open / 32 limit");
  });
});

describe("DockHeader window controls", () => {
  it("offers Find where the panes are on screen (Shell and Files)", () => {
    expect(html({ section: "shell" })).toContain('aria-label="Find in terminal"');
    expect(html({ section: "console" })).not.toContain('aria-label="Find in terminal"');
    expect(html({ section: "files" })).toContain('aria-label="Find in terminal"');
    expect(html({ section: "agents" })).not.toContain('aria-label="Find in terminal"');
    expect(tagOf(html({ findDisabled: false }), 'aria-label="Find in terminal"')).not.toContain("disabled");
    expect(tagOf(html({ findDisabled: true }), 'aria-label="Find in terminal"')).toContain("disabled");
  });

  it("has a labelled overflow button that opens a menu", () => {
    const tag = tagOf(html(), 'data-testid="dock-overflow"');
    expect(tag).toContain('aria-label="Dock options"');
    expect(tag).toContain('aria-haspopup="menu"');
  });

  it("names expand, restore and hide", () => {
    const body = html({ onToggleExpanded: () => {}, onClose: () => {} });
    expect(body).toContain('aria-label="Expand the dock"');
    expect(body).toContain('aria-label="Hide the dock"');
    expect(body).toContain("sessions keep running");
    expect(html({ onToggleExpanded: () => {}, expanded: true })).toContain('aria-label="Restore dock size"');
    expect(html()).not.toContain('aria-label="Hide the dock"');
  });
});

describe("DockHeader overflow menu", () => {
  it("is closed until opened", () => {
    expect(html()).not.toContain("data-dock-overflow-menu");
  });

  it("holds the session list, float toggle, tab options and session search", () => {
    const body = html({
      menuOpen: true,
      canClearScrollback: true,
      canTabOptions: true,
      canFindSession: true,
      onToggleDockMode: () => {},
    });
    expect(body).toContain('role="menu"');
    expect(body).toContain("gp-menu");
    expect(body).toContain("Clear scrollback");
    expect(body).toContain('data-testid="terminal-session-count"');
    expect(body).toContain("All sessions");
    expect(body).toContain("3 open / 32 limit");
    expect(body).toContain("data-terminal-dock-mode-toggle");
    expect(body).toContain("Float over the view");
    expect(body).toContain("Tab options");
    expect(body).toContain("Find session in this repository");
  });

  it("keeps the float toggle's label stable and reports the mode as checked state", () => {
    // A menuitemcheckbox names the option; aria-checked says whether it is on.
    const over = html({ menuOpen: true, dockMode: "overlay", onToggleDockMode: () => {} });
    expect(over).toContain("Float over the view");
    expect(over).not.toContain("Dock under the view");
    expect(tagOf(over, "data-terminal-dock-mode-toggle")).toContain('aria-checked="true"');
    const docked = html({ menuOpen: true, dockMode: "docked", onToggleDockMode: () => {} });
    expect(tagOf(docked, "data-terminal-dock-mode-toggle")).toContain('aria-checked="false"');
  });

  it("puts the destructive Clear item last, after a separator, so the first item is All sessions", () => {
    const body = html({ menuOpen: true, canClearScrollback: true, canTabOptions: true, canFindSession: true, onToggleDockMode: () => {} });
    const order = ["All sessions", "Find session in this repository", "Tab options", "Float over the view", "Clear scrollback"].map((s) => body.indexOf(s));
    expect(order.every((i) => i > -1)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    const seps = [...body.matchAll(/role="separator"/g)].map((m) => m.index ?? 0);
    expect(seps).toHaveLength(2);
    expect(seps[1]).toBeGreaterThan(body.indexOf("Float over the view"));
    expect(seps[1]).toBeLessThan(body.indexOf("Clear scrollback"));
    // No clear item and no float toggle: no separators at all.
    expect(html({ menuOpen: true, section: "agents" })).not.toContain('role="separator"');
  });

  it("shows a trailing Check on the float item only when floating", () => {
    const over = html({ menuOpen: true, dockMode: "overlay", onToggleDockMode: () => {} });
    const docked = html({ menuOpen: true, dockMode: "docked", onToggleDockMode: () => {} });
    expect(over).toContain("lucide-check");
    expect(docked).not.toContain("lucide-check");
    expect(over).not.toContain("lucide-pin-off");
  });

  it("swaps the clear item with the section and drops items that do not apply", () => {
    const consoleBody = html({ menuOpen: true, section: "console", canClearOutput: true });
    expect(consoleBody).toContain("Clear output");
    expect(consoleBody).not.toContain("Clear scrollback");
    expect(html({ menuOpen: true, section: "console", canClearOutput: false })).not.toContain("Clear output");
    const bare = html({ menuOpen: true });
    expect(bare).not.toContain("Tab options");
    expect(bare).not.toContain("Find session in this repository");
    expect(bare).not.toContain("data-terminal-dock-mode-toggle");
  });

  it("counts as inside the overlay dock, restores focus to the button and roves with the arrow keys", () => {
    // The overlay dock closes on an outside pointerdown unless the layer wears
    // one of `ui/panelDismiss.ts`'s floating-layer classes.
    expect(source).toContain("gp-menu gp-pop");
    expect(source).toContain('inside: "[data-dock-overflow], [data-dock-overflow-menu]"');
    expect(source).toContain("restoreFocusTo(overflowEl)");
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain('"ArrowDown"');
    expect(source).toContain('"ArrowUp"');
    expect(source).toContain("use:portal={\"body\"}");
  });
});
