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
  blocked: [], excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 0, filtered: 0 },
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
    const body = html(answer({ suggestions: [], excluded: { in_progress: 2, review: 0, duplicate: 0, blocked: 1, unread: 0, filtered: 0 } }));
    expect(body).toContain("task-suggest-empty");
    expect(body).toContain("3 held back");
  });
  it("never reads a capped board as the whole board", () => {
    expect(html(answer())).not.toContain("task-suggest-partial");
    expect(html(answer({ board: { scanned: 2000, complete: false, unhydrated: 0 } }))).toContain("only 2000 tasks were read");
    expect(html(answer({ board: { scanned: 5, complete: true, unhydrated: 2 } }))).toContain("2 open tasks' links could not be read");
    expect(html(answer({ verification: { checked: 12, unchecked: 8, failed: 0 } }))).toContain("Verification read for 12 of 20");
  });
  it("calls a task blocked only when an open task blocks it, and counts unread tasks apart", () => {
    const unreadOnly = html(answer({ excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 4, filtered: 0 }, board: { scanned: 9, complete: false, unhydrated: 4 } }));
    expect(unreadOnly).not.toContain("blocked by an open task");
    expect(unreadOnly).toContain("4 not checked: their links were not read");
    const real = html(answer({ excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 1, unread: 0, filtered: 0 }, blocked: [{ id: "x", title: "Held task", blocked_by: ["y"] }] }));
    expect(real).toContain("1 blocked by an open task");
    expect(real).toContain("Held task");
    expect(real).toContain("blocked by y");
    expect(real).not.toContain("not checked");
  });
  it("shows the partial warning when nothing could be suggested, not 'no task is ready'", () => {
    const body = html(answer({ suggestions: [], candidates: 0, excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 3, filtered: 0 }, board: { scanned: 5, complete: false, unhydrated: 3 } }));
    expect(body).toContain("task-suggest-empty");
    expect(body).toContain("task-suggest-partial");
    expect(body).toContain("3 open tasks' links could not be read");
    expect(body).not.toContain("No open task is ready to start");
    expect(html(answer({ suggestions: [], candidates: 0 }))).toContain("No open task is ready to start");
  });
  it("offers every priority 0 to 3 and selects the one asked for", () => {
    const body = html(answer());
    for (const level of [0, 1, 2, 3]) expect(body).toContain(`<option value="${level}"`);
    expect(html(answer(), { priority: 3 })).toMatch(/<option value="3" selected/);
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
