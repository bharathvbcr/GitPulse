import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import IssueTaskActions from "./IssueTaskActions.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "IssueTaskActions.svelte"), "utf8");
const panel = readFileSync(join(here, "GitHubPanel.svelte"), "utf8");

const issue = { number: 7, title: "Crash on open", state: "OPEN", url: "https://example.invalid/7", labels: ["bug"], updated_at: "", author: "a" };

describe("IssueTaskActions rendering", () => {
  it("offers Report issue… and, when issues exist, Add all to tasks", () => {
    const { body } = render(IssueTaskActions, { props: { issues: [issue], onReported: () => {} } });
    expect(body).toContain("Report issue…");
    expect(body).toContain("Add all to tasks");
  });

  it("offers no Add all to tasks without issues", () => {
    const { body } = render(IssueTaskActions, { props: { issues: [], onReported: () => {} } });
    expect(body).toContain("Report issue…");
    expect(body).not.toContain("Add all to tasks");
  });

  it("holds Add all to tasks and the per-issue control back until the catalog is read", () => {
    const { body } = render(IssueTaskActions, { props: { issues: [issue], onReported: () => {} } });
    // Not read yet: the paused state is shown and Add all cannot be pressed.
    expect(body).toContain("Try again");
    expect(body).toMatch(/<button[^>]*disabled[^>]*>[\s\S]*?Add all to tasks/);
  });
});

describe("IssueTaskActions is the issue block's task wiring, not a second list", () => {
  it("takes the pane's issues and hands each row its control instead of listing issues itself", () => {
    expect(source).toContain("issues: IssueInfo[];");
    expect(source).toContain("{@render children?.(rowAction)}");
    expect(source).not.toContain("{#each issues");
    expect(source).not.toContain("readGitHubContext");
    expect(source).not.toContain("createAdaptiveTimer");
    expect(panel).toContain("<IssueTaskActions issues={ctx.issues}");
    expect(panel).toContain("{@render rowAction(issue)}");
    // Remote's own rows stay: still the one list.
    expect(panel).toContain("{#each visibleIssues as issue (issue.number)}");
  });

  it("uses the exact copy", () => {
    for (const label of ["Report issue…", "Add to tasks", "Add all to tasks"]) expect(source).toContain(label);
    expect(source).not.toContain("+ Task");
  });

  it("reports an issue through the gated store action and shows the verdict", () => {
    expect(source).toContain("repoStore.reportIssue(title, issueBody, labels)");
    expect(source).toContain("verdictLabel(outcome.policy)");
    expect(source).toContain("onReported();");
    expect(source).toContain("disabled={!issueTitle.trim() || working}");
  });
});

describe("IssueTaskActions task integration", () => {
  it("wires the issue-to-task helpers", () => {
    expect(source).toContain("createTaskFromIssue");
    expect(source).toContain("batchCreateTasksFromIssues");
    expect(source).toContain("findTaskForIssue");
  });

  it("provides individual creation and in-task detection for each issue", () => {
    expect(source).toContain("{@const inTask = findTaskForIssue(repoTasks, issue.number)}");
    expect(source).toContain("onclick={() => addIssueToTask(issue)}");
    expect(source).toContain("In tasks");
  });

  it("confirms, then adds every unimported open issue in one batch", () => {
    expect(source).toContain("addAllIssuesToTasks");
    expect(source).toContain("const unimportedIssues = $derived(issues.filter((issue) => !findTaskForIssue(repoTasks, issue.number)));");
    expect(source).toContain("await askConfirm({");
    expect(source).toContain("batchCreateTasksFromIssues(pending, repoPath, repoTasks)");
  });

  it("navigates to the Tasks board from a row and from the notice", () => {
    expect(source).toContain("function openTasksView()");
    expect(source).toContain('repoStore.setViewSection("work", "tasks")');
    expect(source).toContain("Open Tasks");
  });

  it("queries repository tasks across all statuses via listAllRepositoryTasks", () => {
    expect(source).toContain("const tasks = await listAllRepositoryTasks(registered.id);");
  });

  it("subscribes to workbench-changed with a foreground-gated live loader and tears it down", () => {
    expect(source).toContain("changed: scheduleRefreshRepoTasks");
    expect(source).toContain("canRun: () => !readBackgroundDocument()");
    expect(source).toContain("function scheduleRefreshRepoTasks() { repoTaskLoader.schedule(); }");
    expect(source).toContain("bindForegroundChanges");
    expect(source).toContain("createListenerTracker()");
    expect(source).toContain("listeners.dispose();");
  });

  it("guards against races during asynchronous task reads", () => {
    expect(source).toContain("let repoTasksGeneration = 0;");
    expect(source).toContain("const currentGen = ++repoTasksGeneration;");
    expect(source).toContain("currentGen === repoTasksGeneration");
    expect(source).toContain("if ($repoStore.currentPath === repoPath)");
  });

  it("leaves task reloads to workbench-changed once it is listening, and follows the issue list otherwise", () => {
    // ft-0ff882a6: re-reading every task with every issue refresh repeated what
    // the change listener already does.
    expect(source).toContain("tasksFollowChanges = true;");
    expect(source).toContain("next !== seenIssues && !tasksFollowChanges");
  });

  it("keeps an unread catalog distinct from an empty one and offers a retry", () => {
    expect(source).toContain("let repoTasks = $state<TaskCard[] | null>(null);");
    expect(source).toMatch(/catch \(err\) \{[^}]*repoTasks = null;\s*repoTasksError = failure\("read", "the task list", err\);/s);
    expect(source).not.toMatch(/repoTasks = \[\];/);
    expect(source).toContain("repoTasks === null");
    expect(source).toContain("TASK_CATALOG_UNREAD_MESSAGE");
    expect(source).toContain("onclick={() => loadRepoTasks()}");
    expect(source).toContain("<span>Try again</span>");
    expect(source).toContain("Task import paused");
  });
});
