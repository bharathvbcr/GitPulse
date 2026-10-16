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
    expect(source).toContain("repoStore.lockWorktree(wt.path, target, true)");
    expect(source).toContain("repoStore.unlockWorktree(wt.path, target, true)");
    // Prune, cache sync, lock and unlock are repoStore mutations: the store owns
    // the command (repoStore.test.ts) and the contract test keeps the panel from sending it.
    expect(source).not.toContain("cmd_lock_worktree");
    expect(source).not.toContain("cmd_unlock_worktree");
    expect(source).toContain("repoStore.pruneWorktrees(target)");
    expect(source).toContain("repoStore.worktreeCowSync(wt.path, target)");
    expect(source).not.toContain("cmd_prune_worktree");
    expect(source).not.toContain("cmd_worktree_cow_sync");
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
  // Rendered in harness/branches.html: arming removes nothing; the second
  // click force-removes a scanned dirty worktree after the armed label named
  // "Discard 12 changed files?"; an unscanned one (dirty_files null) says
  // "Not scanned for changes" and is removed without force.
  it("arms a clean worktree's remove without naming a cost it does not have", () => {
    // The fixture has no clean linked worktree; the third branch of the label.
    expect(source).toContain(': "Click again to remove";');
  });

  it("closes the stranded tab instead of leaving it on the removed directory (T-F09)", () => {
    // Any tab on the removed directory, not only the active one: a background
    // tab used to stay open on a deleted path, holding a tab slot.
    const fn = source.slice(source.indexOf("async function remove"), source.indexOf("function open"));
    expect(fn).not.toContain("$repoStore.currentPath === targetPath");
    const removed = fn.indexOf("repoStore.removeWorktree(");
    const closeIdx = fn.indexOf("repoStore.closeTab(stranded.id)");
    const staleReturn = fn.indexOf("repoStore.targetIsActive(target)", removed);
    expect(removed).toBeGreaterThan(-1);
    expect(closeIdx).toBeGreaterThan(removed);
    expect(closeIdx).toBeLessThan(staleReturn);
  });

  it("closes a stranded tab after merge-and-teardown too, active or not", () => {
    // The same directory removal, reached through the merge button, carried
    // the same active-only check.
    const fn = source.slice(source.indexOf("async function mergeTeardown"), source.indexOf("async function fetchAiSummary"));
    expect(fn).not.toContain("$repoStore.currentPath === wt.path");
    const merged = fn.indexOf("repoStore.worktreeMergeTeardown(");
    const closeIdx = fn.indexOf("repoStore.closeTab(stranded.id)");
    const staleReturn = fn.indexOf("repoStore.targetIsActive(target)", closeIdx);
    expect(merged).toBeGreaterThan(-1);
    expect(closeIdx).toBeGreaterThan(merged);
    expect(closeIdx).toBeLessThan(staleReturn);
  });

  it("holds the merge to the commit the row showed, and keeps the tab of a worktree that is still there", () => {
    const fn = source.slice(source.indexOf("async function mergeTeardown"), source.indexOf("async function fetchAiSummary"));
    expect(fn).toMatch(/repoStore\.worktreeMergeTeardown\([\s\S]*?expectedTip: wt\.head/);
    // The tab goes only with a worktree the teardown removed.
    expect(fn).toMatch(/merged\.worktree_removed \? \$repoStore\.openTabs\.find/);
    expect(fn).toContain('worktreeSetupNotices(merged, "merge")');
  });

  it("keeps the concurrent-session guards across the awaits", () => {
    const fn = source.slice(source.indexOf("async function remove"), source.indexOf("function open"));
    // A fresh read of "the repository in front" after the await is what the old
    // currentPath comparison was; the target the confirm captured replaces it.
    expect(fn).not.toContain("$repoStore.currentPath !== repo");
    expect(fn).toMatch(/await repoStore\.trustRepo\(targetPath\)[\s\S]*?repoStore\.removeWorktree\(targetPath, force, target/);
    expect(fn).toMatch(/repoStore\.targetIsActive\(target\)\) return;\s*await load\(\)/);
  });

  it("routes every worktree mutation through repoStore with the target captured before the first await", () => {
    // The store journals (kind, label, ok), counts the call for the quit guard
    // and queues it per repository, so the panel keeps none of that itself.
    expect(source).not.toContain("harnessStore");
    expect(source).not.toMatch(/invoke(<[^>]*>)?\("cmd_(add|remove|lock|unlock)_worktree"/);
    expect(source).not.toContain('"cmd_worktree_merge_teardown"');
    for (const [start, end, wrapper] of [
      ["async function toggleLock", "async function prune", "repoStore.lockWorktree("],
      ["async function create", "function removeArmTitle", "repoStore.addWorktree("],
      ["async function remove", "function open", "repoStore.removeWorktree("],
      ["async function mergeTeardown", "async function fetchAiSummary", "repoStore.worktreeMergeTeardown("],
    ] as const) {
      const body = source.slice(source.indexOf(start), source.indexOf(end));
      const captured = body.indexOf("repoStore.captureTarget()");
      const firstAwait = body.indexOf("await ");
      expect(captured, `${start} captures a target`).toBeGreaterThan(-1);
      expect(captured, `${start} captures before it awaits`).toBeLessThan(firstAwait);
      expect(body, `${start} calls ${wrapper}`).toContain(wrapper);
    }
  });

  it("captures the armed confirm's target on the first click, and refuses to run when it is no longer in front", () => {
    for (const [start, end, armed] of [
      ["async function remove", "function open", "removeTarget = armed;"],
      ["async function mergeTeardown", "async function fetchAiSummary", "mergeTarget = armed;"],
    ] as const) {
      const body = source.slice(source.indexOf(start), source.indexOf(end));
      const arming = body.indexOf(armed);
      const second = body.indexOf("const target = ", arming);
      expect(arming, `${start} arms with a target`).toBeGreaterThan(-1);
      expect(second, `${start} runs on the armed target`).toBeGreaterThan(arming);
    }
    const merge = source.slice(source.indexOf("async function mergeTeardown"), source.indexOf("async function fetchAiSummary"));
    expect(merge).toMatch(/if \(!repoStore\.targetIsActive\(target\)\) \{\s*toastStore\.error\([^)]*repository changed[^)]*\);\s*return;/);
    expect(merge.indexOf("repoStore.targetIsActive(target)")).toBeLessThan(merge.indexOf("repoStore.worktreeMergeTeardown("));
  });

  it("freezes worktree creation inputs before calling the store", () => {
    const body = source.slice(source.indexOf("async function create"), source.indexOf("function removeArmTitle"));
    const call = body.indexOf("repoStore.addWorktree(");
    expect(call, "the create call").toBeGreaterThan(-1);
    for (const declaration of [
      "const targetPath = newPath.trim();",
      "const branch = newBranch.trim();",
      "const base = startPoint.trim();",
    ]) {
      const index = body.indexOf(declaration);
      expect(index, declaration).toBeGreaterThan(-1);
      expect(index, `${declaration} before the call`).toBeLessThan(call);
    }
  });
});

describe("WorktreesPanel store-emission churn guards", () => {
  it("keeps exactly one mount trigger for the worktree list load", () => {
    // The load effect runs on mount; a second onMount(load) double-fetched.
    expect(source).not.toContain("onMount");
    const effect = source.slice(source.indexOf("const generation = $derived($repoStore.generation);"), source.indexOf("async function load"));
    expect(effect).toContain("untrack(() => void load());");
  });

  it("depends only on value-compared keys, so unrelated store emissions are no-ops", () => {
    // The hand-copied memo guard this replaced had to list every value the
    // effect read, and re-typing it is how a term got dropped. Now the effect
    // reads only keys that compare by value — the repository path store, a
    // derived generation, the trust count — and runs the load untracked, so
    // nothing else it touches can re-run it. The browser harness
    // (harness/repo-churn.html) proves the armed confirm survives a publish.
    const effect = source.slice(source.indexOf("const generation = $derived($repoStore.generation);"), source.indexOf("async function load"));
    expect(effect).toContain("void $currentRepoPath;");
    expect(effect).toContain("void generation;");
    expect(effect).toContain("void $trustExtended;");
    expect(effect).not.toMatch(/\bprev[A-Z]\w* =/);
    expect(effect.slice(effect.indexOf("$effect("))).not.toContain("$repoStore.");
  });

  it("resets the armed remove confirm only on real repo/generation change or unmount", () => {
    const effect = source.slice(source.indexOf("const generation = $derived($repoStore.generation);"), source.indexOf("async function load"));
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
    const effect = source.slice(source.indexOf("const generation = $derived($repoStore.generation);"), source.indexOf("async function load"));
    expect(effect).toContain("void $trustExtended;");
    expect(effect).not.toContain("return;");
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

/**
 * Repository hooks consent is rendered in harness/branches.html: a new
 * worktree starts with hooks off and cache sharing on and sends that choice;
 * asking for hooks shows the file's exact text and says nothing runs until it
 * is approved; approving after the file changed on disk sends the text that
 * was shown, not the newer file, and leaves it unapproved; approving the text
 * on screen marks it approved; creating then sends runHooks.
 */
