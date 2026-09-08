import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { createPaneCrashReporter, createPaneDetails } from "./paneCrash";

describe("pane crash evidence", () => {
  it("keeps request details scoped to the current pane owner and repository", () => {
    const details = createPaneDetails();
    const old = details.register("map");
    old.update("/r/one", "files", { graph: 3 });
    const context = { view: "code", section: "map", repo: "/r/one", file: null };
    expect(details.read(context)).toContain('"graph":3');
    expect(details.read({ ...context, repo: "/r/two" })).toBeNull();
    expect(details.read({ ...context, section: "blame" })).toBeNull();
    const next = details.register("map");
    next.update("/r/one", "symbols", { graph: 4 });
    old.dispose();
    old.update("/r/one", "STALE", { graph: 5 });
    expect(details.read(context)).toContain("symbols");
    expect(details.read(context)).not.toContain("STALE");
    next.dispose();
    expect(details.read(context)).toBeNull();
  });
  it("snapshots context and stack before the deferred write", () => {
    let context = { view: "code", section: "map", repo: "/r/one", file: "A.md" };
    const writes: string[] = [];
    const pending: Array<() => void> = [];
    const reporter = createPaneCrashReporter({ error: (_source, message) => writes.push(String(message)) }, () => context, (work) => pending.push(work));
    reporter.observe(context);
    const error = new Error("each_key_duplicate");
    error.stack = "Error: each_key_duplicate\n at RepoMapPanel.svelte:620";
    reporter.report("workspace", error);
    expect(writes).toHaveLength(0);
    context = { ...context, repo: "/r/two", section: "blame" };
    pending[0]();
    expect(writes[0]).toContain("workspace");
    expect(writes[0]).toContain("/r/one");
    expect(writes[0]).not.toContain("/r/two");
    expect(writes[0]).toContain("RepoMapPanel.svelte:620");
  });

  it("bounds and redacts context, history, and hostile stacks", () => {
    const writes: string[] = [];
    const context = { view: "code", section: "map", repo: "https://user:secret@example.test/r", file: "A.md" };
    const reporter = createPaneCrashReporter({ error: (_source, message) => writes.push(String(message)) }, () => context, (work) => work());
    for (let i = 0; i < 10_000; i++) reporter.observe({ ...context, file: `file-${i}.md` });
    reporter.report("workspace", { message: "oops", stack: "Authorization: Bearer private-value\n" + "frame\n".repeat(10_000) + "at RepoMapPanel.svelte:620" });
    expect(writes[0].length).toBeLessThanOrEqual(2000);
    expect(writes[0]).not.toContain("private-value");
    expect(writes[0]).not.toContain("user:secret");
    expect(writes[0]).toContain("file-9999.md");
    expect(writes[0]).toContain("truncated");
    expect(writes[0]).toContain("RepoMapPanel.svelte:620");
    expect(() => reporter.report("workspace", { get stack() { throw new Error("hostile"); } })).not.toThrow();
    expect(writes[1]).toContain("stack unavailable");
  });

  it("records each boundary failure through onerror, never by rendering the fallback", () => {
    const source = readFileSync(new URL("../../App.svelte", import.meta.url), "utf8");
    const boundaries = source.match(/<svelte:boundary\b[^>]*>/g) ?? [];
    expect(boundaries.length).toBeGreaterThanOrEqual(8);
    expect(boundaries.every((tag) => tag.includes("onerror="))).toBe(true);
    expect(source).not.toContain("{reportPaneCrash(error)}");
    expect(source.includes("Details saved in Diagnostics")).toBe(false);
  });
});
