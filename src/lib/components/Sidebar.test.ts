import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import Sidebar from "./Sidebar.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "Sidebar.svelte"),
  "utf8"
);

describe("Sidebar", () => {
  it("labels its icon-only controls for screen readers", () => {
    const { body } = render(Sidebar);
    // Gold standard (FilterBar): every icon-only button carries an explicit
    // aria-label alongside its title.
    expect(body).toContain('aria-label="Open Repository"');
  });

  it("keeps the per-file stage/unstage affordances titled", () => {
    // File rows render from backend state (absent under SSR), so these are
    // asserted at source level like DiffViewer.test.ts does.
    expect(source).toContain('title="Unstage file"');
    expect(source).toContain('title="Stage file"');
  });
});

describe("Sidebar batch staging", () => {
  it("offers stage all and unstage all in the change-list headers", () => {
    expect(source).toContain("stageAll");
    expect(source).toContain("unstageAll");
    expect(source).toContain("stage all");
    expect(source).toContain("unstage all");
  });
});
