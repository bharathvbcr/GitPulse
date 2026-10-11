import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GitHubPanel from "./GitHubPanel.svelte";
import { VIEW_REGISTRY } from "../views/viewRegistry";

// What the panel does with a live page — truncation and error states, the
// narrowing filters, previews and expanders, guarded actions and their
// verdicts, CI:local, the live run poll, cache hydration, layout — is checked in
// harness/github-panel.html on Chrome and WebKit. The three source contracts
// left here pin timer lifecycles that a rendered check cannot reach within a
// harness deadline.
const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "GitHubPanel.svelte"),
  "utf8",
);

describe("GitHubPanel", () => {
  it("renders header and action buttons", () => {
    const { body } = render(GitHubPanel);
    // The heading names the section the reader clicked. Which forge this is
    // stays legible from the mark and the owner/repo link beside it, both of
    // which need a context this render has not got.
    expect(body).toContain("Remote");
    expect(body).toContain("lucide-folder-git-2");
    expect(body).toContain("Run CI locally");
    expect(body).toContain("Refresh");
  });

  it("titles the pane the way the section that opens it is labelled", () => {
    // Header and tab disagreeing about a pane's name is how a reader ends up
    // unsure whether they arrived where they clicked.
    const section = VIEW_REGISTRY.work.sections?.find((s) => s.id === "remote");
    expect(section?.label).toBe("Remote");
    const heading = /<h2[^>]*>([\s\S]*?)<\/h2>/.exec(render(GitHubPanel).body)?.[1] ?? "";
    expect(heading.replace(/<[^>]+>/g, "").trim()).toBe(section?.label);
  });
});

describe("GitHubPanel live-poll lifecycle contracts", () => {
  it("drops a poll whose repository is no longer the open one", () => {
    // A resolve can land after the path moved; applying it would show one
    // repository's runs under another's name.
    expect(source).toContain("if ($repoStore.currentPath !== repo) return false;");
  });

  it("creates the driver before the effect that feeds it, and tears it down per repository", () => {
    // Svelte runs effects in declaration order. If the sync effect ran first,
    // its one call would hit a null driver and no-op — and on a repository
    // whose runs arrive already in flight from the panel cache, `runs` never
    // changes again, so nothing would ever start the poll.
    const driverAt = source.indexOf("createLivePoll({");
    const syncAt = source.indexOf("livePoll?.sync(moving)");
    expect(driverAt).toBeGreaterThan(0);
    expect(syncAt).toBeGreaterThan(0);
    expect(driverAt, "the driver effect must be declared first").toBeLessThan(syncAt);
    expect(source).toContain("driver.dispose()");
  });
});
