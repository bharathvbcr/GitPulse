import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitComposer from "./CommitComposer.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitComposer.svelte"),
  "utf8",
);

describe("CommitComposer", () => {
  it("renders the include-unstaged option and staged-only commit by default", () => {
    const { body } = render(CommitComposer);
    expect(body).toContain("Include unstaged");
    expect(body).toContain('aria-label="Include unstaged files in this commit"');
    expect(body).toContain("Amend");
    expect(body).toContain("Commit (0)");
    expect(body).not.toContain("Commit all");
  });

  it("offers include-unstaged as the quick-commit option", () => {
    expect(source).toContain("includeUnstaged");
    expect(source).toContain("repoStore.quickCommit");
    expect(source).toContain("Commit all");
    expect(source).toContain("onclick={() => void handleCommit()}");
  });

  it("keeps staged-only commit as the default path", () => {
    expect(source).toContain("repoStore.commit(message, isAmending)");
    expect(source).toContain("Amend");
  });

  it("routes Cmd/Ctrl+Enter through the composer and Shift to include unstaged", () => {
    expect(source).toContain("onMessageKeydown");
    expect(source).toContain("isImeComposition");
    expect(source).toContain('event.key !== "Enter"');
    expect(source).toContain("event.shiftKey");
    expect(source).toContain("handleCommit(true)");
    expect(source).toContain("forceQuick");
  });

  it("disables commit when conflicts are present", () => {
    expect(source).toContain("conflictedCount > 0");
  });
});
