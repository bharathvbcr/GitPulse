import { describe, expect, it } from "vitest";
import {
  acceptPreview,
  applyConfirmText,
  canApply,
  hostLabel,
  kindLabel,
  indexPlansByHost,
  INTEGRATION_HOSTS,
  planSummary,
  totalChanges,
} from "./agentIntegration";
import type { IntegrationHost, IntegrationKind, IntegrationPlan } from "../codeintel/types";

function plan(overrides: Partial<IntegrationPlan> = {}): IntegrationPlan {
  return {
    available: true,
    host: "claude",
    repo: "/w/project",
    applied: false,
    reason: null,
    entries: [],
    notes: [],
    repo_changes: 0,
    outside_changes: 0,
    protected: [],
    ...overrides,
  };
}

describe("planSummary", () => {
  it("says a current integration is current", () => {
    expect(planSummary(plan())).toBe("Already registered and current.");
  });

  it("counts repository files on their own when nothing outside changes", () => {
    expect(planSummary(plan({ repo_changes: 3 }))).toBe("3 files in this repository");
    expect(planSummary(plan({ repo_changes: 1 }))).toBe("1 file in this repository");
  });

  it("never folds home-directory writes into the repository count", () => {
    // The consent distinction: agreeing to add guides to a project is not
    // agreeing to edit `~/.claude.json`, and one total would hide that.
    const summary = planSummary(plan({ repo_changes: 3, outside_changes: 1 }));
    expect(summary).toContain("3 files in this repository");
    expect(summary).toContain("1 in your home directory");
    expect(summary).not.toBe("4 files in this repository");
  });

  it("carries the backend's reason when the plan could not be read", () => {
    expect(planSummary(plan({ available: false, reason: "devmap is not installed" }))).toBe(
      "devmap is not installed",
    );
  });

  it("never claims an unavailable plan is current", () => {
    // `repo_changes` is 0 on an unavailable plan because nothing was measured.
    const unavailable = plan({ available: false, reason: null });
    expect(planSummary(unavailable)).not.toContain("current");
    expect(canApply(unavailable)).toBe(false);
  });
});

describe("applyConfirmText", () => {
  it("names the home directory explicitly when it will be written", () => {
    const text = applyConfirmText(plan({ repo_changes: 2, outside_changes: 1 }));
    expect(text).toContain("Claude Code");
    expect(text).toContain("2 files in this repository");
    expect(text).toContain("1 outside it, in your home directory");
  });

  it("does not mention the home directory when nothing outside changes", () => {
    const text = applyConfirmText(plan({ repo_changes: 2 }));
    expect(text).not.toContain("home directory");
  });

  it("warns that repository files become visible in git status", () => {
    expect(applyConfirmText(plan({ repo_changes: 1 }))).toContain("git status");
  });

  it("does not mention git status for a home-directory-only write", () => {
    const text = applyConfirmText(plan({ repo_changes: 0, outside_changes: 1 }));
    expect(text).not.toContain("git status");
    expect(text).toContain("home directory");
  });

  it("says which guides will be left alone", () => {
    const text = applyConfirmText(
      plan({ repo_changes: 1, protected: ["/w/project/AGENTS.md"] }),
    );
    expect(text).toContain("Left alone because you wrote it");
    expect(text).toContain("/w/project/AGENTS.md");
  });

  it("pluralizes protected guides correctly", () => {
    const text = applyConfirmText(
      plan({ repo_changes: 1, protected: ["/w/project/AGENTS.md", "/w/project/CLAUDE.md"] }),
    );
    expect(text).toContain("Left alone because you wrote them");
  });
});

describe("canApply", () => {
  it("is false with nothing to write", () => {
    expect(canApply(plan())).toBe(false);
  });

  it("is true for a home-directory-only change", () => {
    // Still a real write the user may want; the button must not be dead.
    expect(canApply(plan({ outside_changes: 1 }))).toBe(true);
  });

  it("is false when the plan could not be read", () => {
    expect(canApply(plan({ available: false, repo_changes: 3 }))).toBe(false);
  });
});

describe("labels", () => {
  it("labels every integration kind the backend can send", () => {
    const kinds: IntegrationKind[] = ["guide", "project_mcp", "global_mcp", "hook", "skill"];
    for (const kind of kinds) {
      expect(kindLabel(kind)).not.toBe(kind);
    }
  });

  it("distinguishes a project MCP entry from a global one", () => {
    expect(kindLabel("project_mcp")).not.toBe(kindLabel("global_mcp"));
  });

  it("falls back to the raw host id rather than dropping an unknown host", () => {
    expect(hostLabel("claude")).toBe("Claude Code");
    expect(hostLabel("some-future-host")).toBe("some-future-host");
  });
});

describe("totalChanges", () => {
  it("adds both locations", () => {
    expect(totalChanges(plan({ repo_changes: 2, outside_changes: 3 }))).toBe(5);
  });
});

describe("indexPlansByHost", () => {
  it("keys a survey by the host that was asked about", () => {
    const byHost = indexPlansByHost(INTEGRATION_HOSTS, [
      plan({ host: "claude", repo_changes: 1 }),
      plan({ host: "cursor" }),
      plan({ host: "codex", repo_changes: 2 }),
    ]);
    expect([...byHost.keys()]).toEqual(["claude", "cursor", "codex"]);
    expect(byHost.get("codex")?.repo_changes).toBe(2);
  });

  it("cannot produce two rows with the same key from a host-agnostic backend", () => {
    // Measured: a stub that answered every host with the same payload made the
    // keyed `each` throw `each_key_duplicate` and took the panel down.
    const same = [plan({ host: "claude" }), plan({ host: "claude" }), plan({ host: "claude" })];
    const byHost = indexPlansByHost(INTEGRATION_HOSTS, same);
    expect(byHost.size).toBe(3);
    expect([...byHost.keys()]).toEqual(["claude", "cursor", "codex"]);
    // Every host that was not actually answered for is unavailable, not
    // silently given another host's plan.
    expect(byHost.get("cursor")?.available).toBe(false);
    expect(byHost.get("cursor")?.reason).toContain("Cursor");
  });

  it("marks an unanswered host unavailable rather than current", () => {
    const byHost = indexPlansByHost(INTEGRATION_HOSTS, [plan({ host: "claude" })]);
    const cursor = byHost.get("cursor")!;
    expect(cursor.available).toBe(false);
    expect(planSummary(cursor)).not.toContain("current");
    expect(canApply(cursor)).toBe(false);
  });

  it("keeps every requested host even when the backend sends none", () => {
    const byHost = indexPlansByHost(INTEGRATION_HOSTS, []);
    expect(byHost.size).toBe(INTEGRATION_HOSTS.length);
    for (const host of INTEGRATION_HOSTS) {
      expect(byHost.get(host)?.available).toBe(false);
    }
  });
});

describe("acceptPreview", () => {
  it("accepts an answer about the host that was asked for", () => {
    const fresh = plan({ host: "cursor", repo_changes: 4 });
    const result = acceptPreview("cursor" as IntegrationHost, fresh);
    expect(result).toBe(fresh);
  });

  it("refuses an answer about a different host instead of filing it wrong", () => {
    const result = acceptPreview("cursor" as IntegrationHost, plan({ host: "claude" }));
    expect(result).toHaveProperty("mismatch");
    expect((result as { mismatch: string }).mismatch).toContain("Cursor");
    expect((result as { mismatch: string }).mismatch).toContain("Claude Code");
  });
});
