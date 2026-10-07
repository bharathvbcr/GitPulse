import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WorktreesPanel from "./WorktreesPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorktreesPanel.svelte"),
  "utf8"
);

describe("WorktreesPanel", () => {
  it("labels the add-worktree button for screen readers", () => {
    const { body } = render(WorktreesPanel);
    expect(body).toContain('aria-label="Create worktree"');
  });

  it("gives the two-step remove button a spoken label, including the arm state", () => {
    // Rows render from backend data (absent in SSR), so the remove control is
    // asserted at source level like DiffViewer.test.ts does.
    expect(source).toContain("aria-label={removeArmTitle(wt)}");
    expect(source).toContain("Click again to remove");
    expect(source).toContain("Remove this worktree");
  });

  it("drops stale cmd_list_worktrees responses via async guard", () => {
    // Overlapping loads after rapid create/remove must not land out of order:
    // every apply path re-checks the guard captured at trigger time.
    expect(source).toContain("createAsyncGuard()");
    expect(source).toContain("if (!guard.isLive()) return;");
    // A superseded load's finally must not clear the newer load's spinner.
    expect(source).toContain("if (guard.isLive()) isLoading = false;");
  });
});

describe("WorktreesPanel agent worktree affordances", () => {
  it("exposes lock, unlock, and prune commands", () => {
    expect(source).toContain("cmd_lock_worktree");
    expect(source).toContain("cmd_unlock_worktree");
    expect(source).toContain("cmd_prune_worktree");
    expect(source).not.toContain("expire:");
  });

  it("reads task bindings through the shared bounded reader, not one at a time", () => {
    // A serial loop made each worktree wait for the previous one's
    // repository check and ledger lookup.
    expect(source).toContain("loadWorktreeTasks(invoke, repo,");
    expect(source).not.toContain('await invoke<string | null>("cmd_worktree_task"');
  });

  it("reloads when repository status generation changes", () => {
    expect(source).toContain("$repoStore.generation");
    expect(source).toContain("cmd_list_worktrees");
  });

  it("names an agent worktree as such, from the directory layout", () => {
    expect(source).toContain("isAgentWorktree");
    expect(source).toContain("agentKind");
    expect(source).toContain("agentSessionSlug");
  });

  it("labels the agent chip for a person, and a GitPulse task worktree as one", () => {
    // The chip used to print the raw directory name, so GitPulse's own task
    // worktrees read as an agent called "gitpulse".
    expect(source).toContain("{agentKindLabel(kind)}</span>");
    expect(source).toContain("isGitPulseLane(kind) ? 'GitPulse task worktree'");
    expect(source).not.toContain("Agent session ");
    expect(source).toContain("Agent worktree (");
  });

  it("does not create worktrees in GitPulse's task container from the renderer", () => {
    // The task provisioner is the one creator under .gitpulse/worktrees/: it
    // excludes the directory from git status first and names the branch after
    // the attempt. The panel's "agent lane" preset skipped both, nested inside
    // whichever checkout was selected, and cmd_add_worktree now refuses it.
    expect(source).not.toContain("spawnAgentLane");
    expect(source).not.toContain(".gitpulse/worktrees");
    expect(source).not.toContain("Agent Lane");
    expect(source).not.toMatch(/\bZap\b/);
  });
});

describe("WorktreesPanel controls say what they do", () => {
  const header = source.slice(source.indexOf("<FolderGit2 size={11} />"), source.indexOf("{#if showAddForm}"));

  it("gives prune and AI summary different icons", () => {
    expect(header).toContain("<Eraser size={11} />");
    expect(header).not.toContain("<Sparkles");
    const rail = source.slice(source.indexOf("fetchAiSummary(wt)}"));
    expect(rail).toContain("<Sparkles size={10} />");
  });

  it("names open by whether a tab for the worktree is already open", () => {
    // openRepo focuses an existing tab rather than opening another, so "Open
    // in a new tab" was false whenever one was open.
    expect(source).not.toContain("in a new tab");
    expect(source).toContain('title={hasOpenTab(wt) ? "Show tab" : "Open"}');
    expect(source).toMatch(/hasOpenTab = \$derived\(\(wt: WorktreeInfo\) =>\s*\$repoStore\.openTabs\.some\(\(tab\) => sameRepo\(tab\.path, wt\.path/);
  });

  it("states the merge confirm once, inline, not again in the button title", () => {
    expect(source).not.toContain("Click GitMerge icon again");
    expect(source).not.toContain("Click again to confirm merge");
    expect(source.match(/again to confirm/g)?.length).toBe(1);
    expect(source).toContain("Press merge again to confirm.");
  });

  it("explains an empty list and offers the next step", () => {
    expect(source).toContain('data-testid="worktrees-empty"');
    expect(source).toMatch(/onlyMain = \$derived\(\s*!isLoading && !error && worktrees\.length > 0 && worktrees\.every\(\(wt\) => wt\.is_main\)/);
    const empty = source.slice(source.indexOf('data-testid="worktrees-empty"'));
    expect(empty.slice(0, 600)).toContain("onclick={() => (showAddForm = true)}");
  });
});

describe("WorktreesPanel removal safety", () => {
  it("force-removes only a scanned dirty worktree after confirm, never an unscanned one", () => {
    // dirty_files === null means the scan did not run. Coercing that to 0
    // and passing --force is how an agent farm past the scan cap lost work.
    expect(source).toContain(
      "const force = typeof wt.dirty_files === \"number\" && wt.dirty_files > 0;",
    );
    expect(source).not.toContain("const force = (wt.dirty_files ?? 0) === 0;");
    expect(source).not.toMatch(/cmd_remove_worktree[\s\S]{0,120}?force:\s*true/);
    expect(source).toMatch(/cmd_remove_worktree[\s\S]{0,120}?force\s*\}/);
  });

  it("names the discard cost in the armed confirm when files would be lost", () => {
    expect(source).toContain("`Discard ${wt.dirty_files} changed files? Click again to remove`");
    expect(source).toContain("`Discard ${wt.dirty_files} changed files?`");
    expect(source).toContain("Not scanned for changes");
  });

  it("closes the stranded tab instead of leaving it on the removed directory (T-F09)", () => {
    // Any tab on the removed directory, not only the active one: a background
    // tab used to stay open on a deleted path, holding a tab slot.
    const fn = source.slice(source.indexOf("async function remove"), source.indexOf("function open"));
    expect(fn).not.toContain("$repoStore.currentPath === targetPath");
    const removed = fn.indexOf("removeCompleted = true");
    const closeIdx = fn.indexOf("repoStore.closeTab(stranded.id)");
    const staleReturn = fn.indexOf("$repoStore.currentPath !== repo", removed);
    expect(closeIdx).toBeGreaterThan(removed);
    expect(closeIdx).toBeLessThan(staleReturn);
  });

  it("closes a stranded tab after merge-and-teardown too, active or not", () => {
    // The same directory removal, reached through the merge button, carried
    // the same active-only check.
    const fn = source.slice(source.indexOf("async function mergeTeardown"), source.indexOf("async function fetchAiSummary"));
    expect(fn).not.toContain("$repoStore.currentPath === wt.path");
    const merged = fn.indexOf("mergeCompleted = true");
    const closeIdx = fn.indexOf("repoStore.closeTab(stranded.id)");
    const staleReturn = fn.indexOf("$repoStore.currentPath !== repo", merged);
    expect(closeIdx).toBeGreaterThan(merged);
    expect(closeIdx).toBeLessThan(staleReturn);
  });

  it("preserves the concurrent-session currentPath guards after the await", () => {
    const fn = source.slice(source.indexOf("async function remove"), source.indexOf("function open"));
    expect(fn.match(/\$repoStore\.currentPath !== repo/g)?.length).toBe(3);
    expect(fn).toMatch(/await repoStore\.trustRepo\(targetPath\)[\s\S]*?currentPath !== repo[\s\S]*?cmd_remove_worktree/);
    expect(fn).toContain("await load()");
  });

  it("journals settled create/remove calls before stale UI returns", () => {
    for (const [start, end, command] of [
      ["async function create", "function removeArmTitle", "cmd_add_worktree"],
      ["async function remove", "function open", "cmd_remove_worktree"],
    ] as const) {
      const body = source.slice(source.indexOf(start), source.indexOf(end));
      const settled = body.indexOf(command);
      const successJournal = body.indexOf("harnessStore.recordAction", settled);
      const successGuard = body.indexOf("$repoStore.currentPath !== repo", settled);
      expect(successJournal, `${start} success journal`).toBeGreaterThan(settled);
      expect(successJournal, `${start} success before stale return`).toBeLessThan(successGuard);

      const caught = body.indexOf("} catch", successGuard);
      const failureJournal = body.indexOf("harnessStore.recordAction", caught);
      const failureGuard = body.indexOf("$repoStore.currentPath !== repo", caught);
      expect(failureJournal, `${start} failure journal`).toBeGreaterThan(caught);
      expect(failureJournal, `${start} failure before stale return`).toBeLessThan(failureGuard);
    }
  });

  it("freezes worktree creation inputs before invoking the backend", () => {
    const body = source.slice(source.indexOf("async function create"), source.indexOf("function removeArmTitle"));
    const invoke = body.indexOf('("cmd_add_worktree"');
    expect(invoke, "the create call").toBeGreaterThan(-1);
    for (const declaration of [
      "const targetPath = newPath.trim();",
      "const branch = newBranch.trim();",
      "const base = startPoint.trim();",
      "const actionLabel = branch ? `${branch} → ${targetPath}` : targetPath;",
    ]) {
      const index = body.indexOf(declaration);
      expect(index, declaration).toBeGreaterThan(-1);
      expect(index, `${declaration} before invoke`).toBeLessThan(invoke);
    }
    expect(body).toContain("label: actionLabel");
  });
});

describe("WorktreesPanel store-emission churn guards", () => {
  it("keeps exactly one mount trigger for the worktree list load", () => {
    // The load effect runs on mount; a second onMount(load) double-fetched.
    expect(source).not.toContain("onMount");
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("void load();");
  });

  it("memo-guards the load effect so unrelated store emissions are no-ops", () => {
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    const guard = effect.match(/if \(([^)]*)\) return;/)?.[1] ?? "";
    expect(guard, "the load effect must open with a memo guard").not.toBe("");
    // Derived rather than spelled out: every reactive value the effect reads
    // has to appear in the memo key. One that is read but not compared makes
    // the guard a no-op for that value, and the ~6s poll tick publishes on
    // every repository — which is the churn this guard exists to absorb. A
    // literal copy of the condition had to be re-typed each time the effect
    // learned to watch something new, and re-typing it is how a term gets
    // dropped.
    const reads = [...effect.matchAll(/const (\w+) = \$[\w.]+;/g)].map((m) => m[1]);
    expect(reads.length, "the effect must read something").toBeGreaterThan(0);
    for (const name of reads) {
      expect(guard, `${name} is read by the effect but absent from the memo key`).toContain(name);
    }
  });

  it("resets the armed remove confirm only on real repo/generation change or unmount", () => {
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("clearTimeout(confirmTimer)");
    // Clearing the timer without disarming would strand a permanently armed confirm.
    expect(effect).toContain("removingPath = null;");
  });
});

describe("WorktreesPanel legacy-trust extension", () => {
  // The offer itself moved to TrustExtensionBanner, mounted above the branch
  // list so it cannot scroll out of sight; its invariants moved with it, to
  // TrustExtensionBanner.test.ts. What stays here is the half this panel still
  // owns: a grant made up there has to reach these rows.

  it("reloads when trust is extended from outside this panel", () => {
    // The panel used to call `load()` itself from the banner's own handler.
    // With the banner gone, the only thing that can reload these rows is the
    // announcement — and it has to be part of the effect's change check, not
    // merely read by it: the repository and generation are both unchanged
    // across a grant, so an unconsidered count would return early and leave
    // every row showing the gaps the grant just closed.
    expect(source).toContain('import { trustExtended } from "../repos/trustExtension"');
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("const extended = $trustExtended;");
    expect(effect).toMatch(/if \(repo === prevRepoPath &&[\s\S]*?extended === prevExtended\) return;/);
    expect(effect).toContain("prevExtended = extended;");
  });

  it("no longer carries a second copy of the offer", () => {
    // Two banners gated on two inspections of the same question is the state
    // this refactor exists to avoid; leaving the old one behind would look
    // like it worked.
    expect(source).not.toContain("trustExtendable");
    expect(source).not.toContain("cmd_repository_trust");
  });
});

describe("WorktreesPanel GitPulse task worktrees", () => {
  it("names a task worktree's task and run state, links back to it, and reads runs only when one is listed", () => {
    // Read once per load, gated on a GitPulse lane being listed, so a
    // repository without one sends no run read (the uncommitted harness pins
    // the IPC surface).
    expect(source).toContain("if (!list.some((wt) => isGitPulseLane(agentKind(wt.path)))) {");
    expect(source).toContain("const { runs } = await listAllLiveRuns();");
    expect(source).toContain("await Promise.all([loadTaskState(repo, next, guard), loadLaneRuns(next, guard)]);");
    expect(source).toContain("liveRunIn(wt.path, liveRuns)");
    expect(source).toContain('data-testid="worktree-task"');
    expect(source).toContain("await openTaskForRun(run.id);");
    expect(source).toContain("{lane.task_title || \"Task\"} · {runStateLabel(lane.state)}");
  });
});
