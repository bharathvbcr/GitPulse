import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import EmptyState from "./EmptyState.svelte";
import { FolderOpen, Plus } from "lucide-svelte";

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
});
