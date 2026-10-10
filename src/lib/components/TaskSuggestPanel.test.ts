import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TaskSuggestPanel from "./TaskSuggestPanel.svelte";
import type { TaskSuggestions } from "../workbench/client";

const answer = (over: Partial<TaskSuggestions> = {}): TaskSuggestions => ({
  generated_at: 1, candidates: 3,
  suggestions: [
    { rank: 1, id: "a", title: "Fix the crash", status: "ready", priority: 0, score: 130, reason: "urgent priority; overdue by 3 days", signals: [] },
    { rank: 2, id: "b", title: "Tidy docs", status: "inbox", priority: 3, score: 4, reason: "no urgency signal; ranked by tie-break", signals: [] },
  ],
  blocked: [], excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, filtered: 0 },
  board: { scanned: 3, complete: true, unhydrated: 0 }, verification: { checked: 2, unchecked: 0, failed: 0 },
  ...over,
});
const props = { loading: false, error: "", priority: "all" as const, onPriority() {}, onOpen() {}, onRefresh() {}, onClose() {} };
const html = (result: TaskSuggestions | null, extra: Record<string, unknown> = {}) => render(TaskSuggestPanel, { props: { ...props, result, ...extra } }).body;

describe("TaskSuggestPanel", () => {
  it("lists the ranked tasks in order, each with its reason and a priority chip", () => {
    const body = html(answer());
    expect(body.indexOf("Fix the crash")).toBeLessThan(body.indexOf("Tidy docs"));
    expect(body).toContain("urgent priority; overdue by 3 days");
    expect(body).toContain('data-task-id="a"');
    expect(body).toContain(">Urgent<");
    expect(body).toContain(">Low<");
    expect(body.match(/data-testid="task-suggest-item"/g)).toHaveLength(2);
  });
  it("says so when nothing is ready, and how many were held back", () => {
    const body = html(answer({ suggestions: [], excluded: { in_progress: 2, review: 0, duplicate: 0, blocked: 1, filtered: 0 } }));
    expect(body).toContain("task-suggest-empty");
    expect(body).toContain("3 held back");
  });
  it("never reads a capped board as the whole board", () => {
    expect(html(answer())).not.toContain("task-suggest-partial");
    expect(html(answer({ board: { scanned: 2000, complete: false, unhydrated: 0 } }))).toContain("only 2000 tasks were read");
    expect(html(answer({ board: { scanned: 5, complete: true, unhydrated: 2 } }))).toContain("2 tasks' links could not be read");
    expect(html(answer({ verification: { checked: 12, unchecked: 8, failed: 0 } }))).toContain("Verification read for 12 of 20");
  });
  it("shows a failure and an unanswered state instead of a stale list", () => {
    expect(html(null, { error: "Manvi is not running" })).toContain("Manvi is not running");
    expect(html(null, { loading: true })).toContain("Ranking the open tasks");
    expect(html(answer(), { error: "boom" })).not.toContain("Fix the crash");
  });
  it("selects the asked priority and disables the controls while it asks", () => {
    expect(html(answer(), { priority: 1 })).toMatch(/<option value="1" selected/);
    expect(html(answer(), { loading: true })).toContain("Asking Manvi");
  });
});
