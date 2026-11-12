import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import EmptyState from "./EmptyState.svelte";
import { FolderOpen, Plus } from "@lucide/svelte";

describe("EmptyState", () => {
  it("renders title and hint", () => {
    const { body } = render(EmptyState, {
      props: {
        icon: FolderOpen,
        title: "No stashes yet",
        hint: "Your stash list is currently empty.",
      },
    });
    expect(body).toContain("No stashes yet");
    expect(body).toContain("Your stash list is currently empty.");
  });

  it("renders action button when action prop is provided", () => {
    const { body } = render(EmptyState, {
      props: {
        icon: FolderOpen,
        title: "No stashes yet",
        action: {
          label: "Stash working tree",
          onClick: () => {},
          icon: Plus,
        },
      },
    });
    expect(body).toContain("Stash working tree");
    expect(body).toContain("gp-btn-primary");
  });

  const rendered = (action: Record<string, unknown>) =>
    render(EmptyState, {
      props: { icon: FolderOpen, title: "No code map", action: { label: "Build index", onClick: () => {}, ...action } },
    }).body;

  it("leaves an ordinary action enabled, untitled and not busy", () => {
    const body = rendered({});
    expect(body).not.toContain("disabled");
    expect(body).not.toContain("title=");
    expect(body).not.toContain("aria-busy");
  });

  it("disables the action and keeps the reason as its title", () => {
    const body = rendered({ disabled: true, title: "Open a repository first" });
    expect(body).toMatch(/<button[^>]*\bdisabled\b/);
    expect(body).toContain('title="Open a repository first"');
  });

  it("marks a busy action aria-busy, swaps its icon for a spinner and disables it", () => {
    const idle = rendered({ icon: Plus });
    const busy = rendered({ icon: Plus, busy: true, label: "Building…" });
    expect(busy).toContain('aria-busy="true"');
    expect(busy).toMatch(/<button[^>]*\bdisabled\b/);
    expect(busy).toContain("animate-spin");
    expect(busy).toContain("Building…");
    expect(idle).not.toContain("animate-spin");
  });
});
