import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import BranchList from "./BranchList.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchList.svelte"),
  "utf8"
);

describe("BranchList", () => {
  it("labels the create-branch button for screen readers", () => {
    const { body } = render(BranchList);
    expect(body).toContain('aria-label="Create branch"');
  });

  it("labels the sparkles button, which only mounts once the create form opens", () => {
    expect(source).toContain('aria-label="Suggest branch name"');
    // The button stays disabled while a suggestion is pending…
    expect(source).toContain("disabled={suggesting}");
    // …and the handler re-checks in flight: two same-tick clicks cannot fire
    // two racing AI invokes before Svelte flushes the disabled attribute.
    expect(source).toContain("if (suggesting || !$repoStore.currentPath) return;");
  });
});
