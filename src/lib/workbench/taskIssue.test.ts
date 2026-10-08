import { describe, expect, it, vi } from "vitest";
import type { Task, TaskCard } from "./client";
import { isIssueInTask, issueLinkLabel, linkedIssueNumber } from "./issueTask";
import { MAX_TASK_LABELS } from "./taskActions";
import {
  issueNumberFromOutput,
  issueUrlFromOutput,
  linkedLabels,
  linkTaskToIssue,
  MAX_TASK_ISSUE_BODY_BYTES,
  MAX_TASK_ISSUE_TITLE,
  prepareTaskIssues,
  runTaskIssues,
  summarizeTaskIssues,
  taskIssueConfirmation,
  taskIssueDraft,
  taskIssuesConfirmation,
  type TaskIssueTarget,
} from "./taskIssue";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function task(over: Partial<Task> = {}): Task {
  return {
    id: "t1", revision: 3, updated_at: 1, title: "Fix the flaky watcher", kind: "bug", status: "ready",
    priority: 2, severity: null, owner: "sam", due_at: null, labels: ["p1", "inbox"],
    repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1, archived: false, completed_at: null, checklist: [], links: [],
    description: "The watcher drops events.", acceptance_criteria: ["No dropped events", "A regression test"],
    ...over,
  };
}

const BIDI = [0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069];
const isControl = (code: number) => code < 0x20 || (code >= 0x7f && code <= 0x9f);

/**
 * `validate_issue_payload` (src-tauri/src/github/mod.rs), restated. A draft
 * this module builds must never be one the backend refuses, or the menu row
 * would offer an action that fails after the reader confirmed it.
 */
function backendRefusal(title: string, body: string): string | null {
  const t = title.trim();
  if (!t) return "empty title";
  if (Array.from(t).length > 256) return "title too long";
  if (Array.from(t).some((c) => isControl(c.codePointAt(0)!))) return "title control";
  if (Array.from(t).some((c) => BIDI.includes(c.codePointAt(0)!))) return "title bidi";
  if (new TextEncoder().encode(body).length > 64 * 1024) return "body too long";
  if (Array.from(body).some((c) => c !== "\n" && c !== "\r" && c !== "\t" && isControl(c.codePointAt(0)!))) return "body control";
  return null;
}

describe("taskIssueDraft", () => {
  it("carries title, description and criteria as a checklist — never labels, owner or the brief", () => {
    const draft = taskIssueDraft(task());
    expect(draft.title).toBe("Fix the flaky watcher");
    expect(draft.body).toContain("The watcher drops events.");
    expect(draft.body).toContain("## Acceptance criteria");
    expect(draft.body).toContain("- [ ] No dropped events");
    expect(draft.body).toContain("- [ ] A regression test");
    expect(draft.body).not.toMatch(/\bp1\b|\binbox\b|\bsam\b/);
    expect(draft.clipped).toBe(false);
  });

  it("says when there is no description instead of filing an empty body", () => {
    const draft = taskIssueDraft(task({ description: "  ", acceptance_criteria: [] }));
    expect(draft.body).toContain("_No description._");
    expect(draft.body).not.toContain("## Acceptance criteria");
  });

  it("strips what the backend refuses from the title and names an empty one", () => {
    expect(taskIssueDraft(task({ title: "a‮b\u0000c\u0085d" })).title).toBe("ab c d");
    expect(taskIssueDraft(task({ title: "\u0001⁦ " })).title).toBe("Untitled task");
  });

  it("clips the title at the backend's code-point limit, not UTF-16 length", () => {
    const title = taskIssueDraft(task({ title: "😀".repeat(400) })).title;
    expect(Array.from(title).length).toBeLessThanOrEqual(MAX_TASK_ISSUE_TITLE);
    expect(title.endsWith("…")).toBe(true);
    expect(title).not.toMatch(/[\uD800-\uDBFF]$|[\uD800-\uDBFF]…$/);
  });

  it("clips an oversized body below the limit and discloses it", () => {
    const draft = taskIssueDraft(task({ description: "é".repeat(200_000) }));
    expect(draft.clipped).toBe(true);
    expect(new TextEncoder().encode(draft.body).length).toBeLessThanOrEqual(MAX_TASK_ISSUE_BODY_BYTES);
    expect(draft.body).toContain("clipped");
  });

  it("folds a multi-line criterion into one checklist item", () => {
    const draft = taskIssueDraft(task({ acceptance_criteria: ["one\n- [x] forged", "", "  "] }));
    expect(draft.body).toContain("- [ ] one - [x] forged");
    expect(draft.body.match(/^- \[/gm)).toHaveLength(1);
  });

  it("never builds a payload the backend validator refuses", () => {
    // Every control character, every bidi override, astral text and sizes
    // straddling both limits.
    const nasty = [
      ...Array.from({ length: 0xa0 }, (_, c) => String.fromCodePoint(c)),
      ...BIDI.map((c) => String.fromCodePoint(c)),
      "😀", "\r\n", "x",
    ];
    let seed = 7;
    const pick = () => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return nasty[seed % nasty.length]; };
    for (let round = 0; round < 300; round += 1) {
      const len = [0, 1, 255, 256, 257, 900][round % 6];
      const title = Array.from({ length: len }, pick).join("");
      const description = Array.from({ length: round % 2 ? 40 : 70_000 }, pick).join("");
      const criteria = [Array.from({ length: 20 }, pick).join("")];
      const draft = taskIssueDraft(task({ title, description, acceptance_criteria: criteria }));
      expect(backendRefusal(draft.title, draft.body), `round ${round}`).toBeNull();
    }
  });
});

describe("issue number and URL from gh's reply", () => {
  it("reads the URL gh prints last", () => {
    const out = "Creating issue in o/r\n\nhttps://github.com/o/r/issues/123\n";
    expect(issueNumberFromOutput(out)).toBe(123);
    expect(issueUrlFromOutput(out)).toBe("https://github.com/o/r/issues/123");
    expect(issueNumberFromOutput("https://ghe.example.com/o/r/issues/9")).toBe(9);
  });

  it("refuses anything that is not an issue URL on the last line", () => {
    for (const out of [
      "", null, undefined, 42, "issue 12 created", "https://github.com/o/r/pull/12",
      "https://github.com/o/r/issues/12\nwarning: something", "https://github.com/o/r/issues/0",
      "javascript:alert(1)//x/y/issues/3", "https://github.com/o/r/issues/12abc",
    ]) {
      expect(issueNumberFromOutput(out)).toBeNull();
      expect(issueUrlFromOutput(out)).toBeNull();
    }
  });
});

describe("the link between a filed task and its issue", () => {
  it("writes the label the import side reads back, so the issue is never imported as a second task", () => {
    for (const n of [1, 42, 99_999]) {
      const linked = linkedLabels(["p1"], n);
      expect(linked.ok).toBe(true);
      const card = { title: "Fix it", labels: linked.ok ? linked.labels : [] };
      expect(isIssueInTask(card, n)).toBe(true);
      expect(linkedIssueNumber(card)).toBe(n);
    }
  });

  it("finds an imported task's issue from its title prefix and ignores cross-references", () => {
    expect(linkedIssueNumber({ title: "[#7] Crash on launch", labels: [] })).toBe(7);
    expect(linkedIssueNumber({ title: "[#100] Fix bug referencing #42", labels: [] })).toBe(100);
    expect(linkedIssueNumber({ title: "Fix bug referencing #42", labels: [] })).toBeNull();
    expect(linkedIssueNumber({ title: "Plain", labels: ["issue-abc", "issue-0", "p1"] })).toBeNull();
    expect(linkedIssueNumber({ title: "Plain", labels: ["#12"] })).toBe(12);
  });

  it("is idempotent and refuses at the label cap rather than silently dropping the link", () => {
    expect(linkedLabels([issueLinkLabel(5)], 5)).toEqual({ ok: true, labels: ["issue-5"] });
    const full = Array.from({ length: MAX_TASK_LABELS }, (_, i) => `l${i}`);
    const refused = linkedLabels(full, 5);
    expect(refused.ok).toBe(false);
  });
});

describe("linkTaskToIssue", () => {
  it("writes against the read revision and reports success off the saved record", async () => {
    const put = vi.fn(async (input: Record<string, unknown>) => task({ revision: 4, labels: input.labels as string[] }));
    const result = await linkTaskToIssue(task(), 12, { put });
    expect(result.ok).toBe(true);
    const sent = put.mock.calls[0][0];
    expect(sent.expected_revision).toBe(3);
    expect(sent.labels).toEqual(["p1", "inbox", "issue-12"]);
  });

  it("does not claim a link the saved task does not carry", async () => {
    const put = vi.fn(async () => task({ revision: 4 }));
    const result = await linkTaskToIssue(task(), 12, { put });
    expect(result).toEqual({ ok: false, reason: "the saved task does not carry the link label" });
  });

  it("returns the store's refusal, e.g. a task edited while the confirmation was open", async () => {
    const put = vi.fn(async () => { throw new Error("revision conflict"); });
    const result = await linkTaskToIssue(task(), 12, { put });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("revision conflict");
  });

  it("writes nothing when the task is already at the label cap", async () => {
    const put = vi.fn();
    const labels = Array.from({ length: MAX_TASK_LABELS }, (_, i) => `l${i}`);
    const result = await linkTaskToIssue(task({ labels }), 12, { put });
    expect(result.ok).toBe(false);
    expect(put).not.toHaveBeenCalled();
  });
});

describe("taskIssueConfirmation", () => {
  it("names the repository, the checkout, and when that checkout is only inferred", () => {
    const draft = taskIssueDraft(task());
    const open = taskIssueConfirmation({ draft, repositoryName: "GitPulse", checkout: "/w/gp", derived: false });
    expect(open).toContain("Fix the flaky watcher");
    expect(open).toContain("GitHub remote of GitPulse, read from /w/gp?");
    expect(open).not.toContain("inferred");
    const derived = taskIssueConfirmation({ draft, repositoryName: "GitPulse", checkout: "/w/gp", derived: true });
    expect(derived).toContain("inferred from the repository's git directory");
  });
});

describe("filing several tasks", () => {
  const gp: TaskIssueTarget = { repositoryName: "GitPulse", checkout: "/w/gp", derived: false };
  const manvi: TaskIssueTarget = { repositoryName: "Manvi", checkout: "/w/manvi", derived: true };
  const asCard = (t: Task): TaskCard => { const { description: _d, acceptance_criteria: _a, ...card } = t; return card; };
  const fixtures = [task({ id: "a", title: "Alpha" }), task({ id: "b", title: "Beta", primary_repository_id: "m" }), task({ id: "c", title: "Gamma" })];
  const read = async (id: string) => structuredClone(fixtures.find((t) => t.id === id)!);
  const resolve = (card: TaskCard) => (card.primary_repository_id === "m" ? manvi : gp);
  const prepared = (ids: string[]) => prepareTaskIssues(fixtures.filter((t) => ids.includes(t.id)).map(asCard), { resolve, read });
  const reply = (n: number) => ({ ok: true, output: `https://github.com/o/r/issues/${n}\n` });
  const linkOk: typeof linkTaskToIssue = async (t, n) => ({ ok: true, task: { ...t, labels: [...t.labels, issueLinkLabel(n)] } });

  it("skips linked, unresolvable and unreadable tasks with a reason, and drafts the rest in order", async () => {
    const cards = [
      asCard(task({ id: "x", title: "Linked", labels: ["issue-4"] })),
      asCard(task({ id: "y", title: "Nowhere", primary_repository_id: "gone" })),
      asCard(task({ id: "z", title: "Vanished" })),
      ...fixtures.map(asCard),
      asCard(fixtures[0]),
    ];
    const { ready, skipped } = await prepareTaskIssues(cards, {
      resolve: (card) => (card.primary_repository_id === "gone" ? "its repository is not loaded" : resolve(card)),
      read: async (id) => { if (id === "z") throw new Error("not found"); return read(id); },
    });
    expect(ready.map((r) => r.task.id)).toEqual(["a", "b", "c"]);
    expect(skipped).toEqual([
      { title: "Linked", reason: "already linked to #4" },
      { title: "Nowhere", reason: "its repository is not loaded" },
      { title: "Vanished", reason: expect.stringContaining("could not be read") },
    ]);
  });

  it("re-checks the link on the fresh read, so a task linked since the board loaded is skipped", async () => {
    const { ready, skipped } = await prepareTaskIssues([asCard(fixtures[0])], {
      resolve, read: async () => ({ ...fixtures[0], labels: ["issue-12"] }),
    });
    expect(ready).toHaveLength(0);
    expect(skipped[0].reason).toBe("already linked to #12");
  });

  it("asks once, grouped by the remote each issue goes to, naming what it will not file", async () => {
    const { ready } = await prepared(["a", "b", "c"]);
    const text = taskIssuesConfirmation(ready, [{ title: "Linked", reason: "already linked to #4" }]);
    expect(text.startsWith("Create 3 issues on GitHub?")).toBe(true);
    expect(text).toMatch(/GitPulse — remote read from \/w\/gp:\n• Alpha\n• Gamma/);
    expect(text).toContain("Manvi — remote read from /w/manvi (inferred");
    expect(text).toContain("Not filed (1):\n• Linked — already linked to #4");
    expect(text).toContain("stops at the first one");
  });

  // A created-but-unlinked task carries no link label, so only this stops a
  // re-run filing it twice.
  it("skips a task whose issue an earlier run created but could not link", async () => {
    const read = vi.fn(async (id: string) => structuredClone(fixtures.find((t) => t.id === id)!));
    const { ready, skipped } = await prepareTaskIssues(fixtures.slice(0, 2).map(asCard), {
      resolve, read, unlinked: (id) => (id === "a" ? 40 : null),
    });
    expect(ready.map((r) => r.task.id)).toEqual(["b"]);
    expect(skipped).toEqual([{ title: "Alpha", reason: "issue #40 already exists for it; link it instead" }]);
    expect(read).not.toHaveBeenCalledWith("a");
  });

  it("says 'issue', not 'issues', when one task is left to file beside skipped ones", async () => {
    const { ready } = await prepared(["a"]);
    const text = taskIssuesConfirmation(ready, [{ title: "Linked", reason: "already linked to #4" }]);
    expect(text.startsWith("Create 1 issue on GitHub?")).toBe(true);
    expect(text).not.toContain("1 issues");
  });

  it("reads exactly like the single confirmation for a run of one", async () => {
    const { ready } = await prepared(["a"]);
    expect(taskIssuesConfirmation(ready, [])).toBe(taskIssueConfirmation({ draft: ready[0].draft, ...gp }));
  });

  it("creates in order and links each task to its own issue", async () => {
    const { ready } = await prepared(["a", "b", "c"]);
    const created: string[] = [];
    const results = await runTaskIssues(ready, {
      create: async (entry) => { created.push(`${entry.target.checkout}:${entry.draft.title}`); return reply(10 + created.length); },
      link: linkOk,
    });
    expect(created).toEqual(["/w/gp:Alpha", "/w/manvi:Beta", "/w/gp:Gamma"]);
    expect(results.map((r) => r.state === "linked" && r.number)).toEqual([11, 12, 13]);
    expect(summarizeTaskIssues(results, [], false)).toEqual({ ok: true, message: "Created and linked 3 issues (#11, #12, #13)." });
  });

  it("stops at a refusal and files nothing after it", async () => {
    const { ready } = await prepared(["a", "b", "c"]);
    const create = vi.fn(async () => (create.mock.calls.length === 2 ? { ok: false, error: "gh: auth required" } : reply(5)));
    const results = await runTaskIssues(ready, { create, link: linkOk });
    expect(create).toHaveBeenCalledTimes(2);
    expect(results.map((r) => r.state)).toEqual(["linked", "refused", "not_attempted"]);
    const summary = summarizeTaskIssues(results, [], false);
    expect(summary.ok).toBe(false);
    expect(summary.message).toContain("Not created — “Beta”: gh: auth required");
    expect(summary.message).toContain("Stopped there; 1 more task was not filed.");
  });

  // The honesty case: gh can publish and then miss its deadline. Calling that
  // "not created" invites the re-run that files a duplicate.
  it("never calls a creation that hit its deadline 'not created', and names the title to look for", async () => {
    const { ready } = await prepared(["a", "b"]);
    const results = await runTaskIssues(ready, { create: async () => ({ ok: false, error: "gh timed out after 90s" }), link: linkOk });
    expect(results.map((r) => r.state)).toEqual(["unknown", "not_attempted"]);
    const { message } = summarizeTaskIssues(results, [], false);
    expect(message).not.toContain("Not created");
    expect(message).toContain("Could not confirm whether “Alpha” was filed");
    expect(message).toContain("Check GitHub for an issue titled “Alpha”");
  });

  it("treats a thrown creation like a reported one, classified the same way", async () => {
    const { ready } = await prepared(["a"]);
    const results = await runTaskIssues(ready, { create: async () => { throw new Error("gh timed out after 90s"); }, link: linkOk });
    expect(results[0].state).toBe("unknown");
  });

  it("stops when a created issue cannot be linked, keeping its number for a retry", async () => {
    const { ready } = await prepared(["a", "b"]);
    const results = await runTaskIssues(ready, {
      create: async () => reply(40),
      link: async () => ({ ok: false, reason: "Task changed" }),
    });
    expect(results[0]).toMatchObject({ state: "unlinked", taskId: "a", number: 40, url: "https://github.com/o/r/issues/40" });
    expect(results[1].state).toBe("not_attempted");
    expect(summarizeTaskIssues(results, [], false).message).toContain("Created issue #40 for “Alpha”, but the task is not linked: Task changed.");
  });

  it("stops when gh's reply names no issue, without guessing a link", async () => {
    const { ready } = await prepared(["a"]);
    const link = vi.fn(linkOk);
    const results = await runTaskIssues(ready, { create: async () => ({ ok: true, output: "done" }), link });
    expect(results[0]).toMatchObject({ state: "unlinked", number: null });
    expect(link).not.toHaveBeenCalled();
    expect(summarizeTaskIssues(results, [], false).message).toContain("check GitHub before filing it again");
  });

  it("honours Stop before the next creation and says the reader stopped it", async () => {
    const { ready } = await prepared(["a", "b", "c"]);
    let stop = false;
    const progress: number[] = [];
    const results = await runTaskIssues(ready, {
      create: async () => { stop = true; return reply(1); },
      link: linkOk,
      stopped: () => stop,
      progress: (index) => progress.push(index),
    });
    expect(progress).toEqual([1]);
    expect(results.map((r) => r.state)).toEqual(["linked", "not_attempted", "not_attempted"]);
    expect(summarizeTaskIssues(results, [], true).message).toContain("Stopped as asked; 2 tasks were not filed.");
  });

  it("reports skipped tasks alongside a clean run, and an all-skipped run as nothing filed", () => {
    expect(summarizeTaskIssues([], [{ title: "Linked", reason: "already linked to #4" }], false))
      .toEqual({ ok: false, message: "Skipped 1 task: “Linked” (already linked to #4)." });
    expect(summarizeTaskIssues([], [], false)).toEqual({ ok: false, message: "No task was filed." });
  });
});
