import "../../../src/app.css";
import { mount, tick, unmount } from "svelte";
import { mockIPCWithEvents } from "../../../harness/tauriMocks";
import { repoStore } from "../../../src/lib/stores/repoStore";
import { conflictSessions } from "../../../src/lib/diff/conflictSession";
import { openConflicts } from "../../../src/lib/ui/workConflicts";
import ConflictEditor from "../../../src/lib/components/ConflictEditor.svelte";
import WorkOverviewPane from "../../../src/lib/components/WorkOverviewPane.svelte";
import { makeRepoFixtures } from "../helpers/fixtures";

const params = new URLSearchParams(location.search);
const scenario = params.get("scenario") ?? "clean";
const width = Number(params.get("width") ?? "1280");
const theme = params.get("theme") === "light" ? "light" : "dark";
document.documentElement.classList.toggle("light", theme === "light");
document.documentElement.classList.toggle("dark", theme === "dark");

const stage = document.getElementById("stage")!;
stage.style.width = `${width}px`;

const crashes: string[] = [];
const unexpected: string[] = [];
window.addEventListener("error", (event) => crashes.push(event.message));
window.addEventListener("unhandledrejection", (event) => crashes.push(String(event.reason)));

const REPO = "/workspace/GitPulse";
let activeMode: "clean" | "dirty" | "conflicted" | "parked" =
  scenario === "dirty"
    ? "dirty"
    : scenario === "conflicted"
      ? "conflicted"
      : "clean";

const getFixture = () => makeRepoFixtures(activeMode, REPO);

mockIPCWithEvents((cmd, args) => {
  if (cmd.startsWith("plugin:")) return null;
  const payload = (args ?? {}) as Record<string, unknown>;
  const fx = getFixture();
  const handlers: Record<string, () => unknown> = {
    cmd_resolve_repo: () => ({
      path: String(payload.repoPath ?? payload.path ?? REPO),
      name: "GitPulse",
      is_bare: false,
      common_dir: `${REPO}/.git`,
    }),
    cmd_list_branches: () => [
      {
        name: "main",
        is_current: true,
        is_remote: false,
        remote_name: null,
        tip_commit_id: "a".repeat(40),
        is_default: true,
        is_gone: false,
        last_commit_timestamp: Math.floor(Date.now() / 1000) - 600,
        last_author: "Alex",
        last_summary: "Work",
        upstream: "origin/main",
        ahead_count: 0,
        behind_count: 0,
      },
    ],
    cmd_get_status: () => fx.statuses,
    cmd_repo_operation: () => fx.operation,
    cmd_list_tags: () => ({ tags: [], truncated: false }),
    cmd_stash_list: () => ({ entries: [], truncated: false }),
    cmd_watch_repo: () => null,
    cmd_unwatch_repo: () => null,
    cmd_last_fetch_at: () => null,
    cmd_note_tab_activated: () => null,
    cmd_get_commit_graph: () => ({ nodes: [], edges: [], total_commits: 0, truncated: false }),
    cmd_set_recent_menu: () => null,
    cmd_branch_stats: () => ({ updates: [], capped: false, compute_failures: 0, compared_to: "main" }),
    cmd_workspace_sync: () => ({ repos: [] }),
    cmd_list_repo_files: () => fx.statuses.map((s) => s.path),
    cmd_task_view: () => ({ available: false, store_path: "", leases: [], error: "" }),
    cmd_worktree_task: () => null,
    cmd_list_worktrees: () => fx.worktrees,
    cmd_github_context: () => ({
      available: false,
      cli_present: false,
      owner: "",
      repo: "",
      pull_requests: [],
      workflow_runs: [],
      warnings: [],
      prs_truncated: false,
      runs_truncated: false,
      issues: [],
      releases: [],
      error: null,
    }),
    cmd_ledger_tail: () => [],
    cmd_grants_view: () => ({ available: false, path: "", grants: [], error: "" }),
    cmd_collision_risk: () => ({
      ok: true,
      error: "",
      overlapping_files: 0,
      worktrees_involved: 0,
      scanned_worktrees: 1,
      unscanned_worktrees: 0,
      failed_worktrees: 0,
      truncated: false,
      items: [],
      shared_worktree_files: 0,
      sessions_ok: true,
      sessions_error: "",
      shared_worktrees: [],
    }),
    cmd_devcouncil_init: () => ({
      repo: REPO,
      state_dir: `${REPO}/.devmap`,
      exclude: { status: "already_ignored", source: "harness" },
      workspace_registry: null,
      workspace_reason: null,
      skipped_untrusted: [],
      skipped_unavailable: [],
      devmap_available: false,
    }),
    cmd_devmap_maybe_refresh: () => ({
      decision: "skip_unavailable",
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: null,
    }),
    cmd_conflict_snapshot: () => {
      const filePath = String(payload.filePath ?? "src/conflict.ts");
      return fx.conflictDocs[filePath] ?? makeRepoFixtures("conflicted", REPO).conflictDocs["src/conflict.ts"];
    },
    cmd_preview_conflict: () => "export const mode = 'ours';",
    cmd_get_file_diff: () => ({
      text: "diff --git a/src/app.css b/src/app.css\n",
      truncated: false,
      truncation_reason: null,
    }),
  };
  if (handlers[cmd]) return handlers[cmd]();
  unexpected.push(cmd);
  return null;
});

const settle = async (ms = 40) => {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, ms));
  await tick();
};

const until = async (pred: () => boolean, label: string, timeoutMs = 4000) => {
  const end = Date.now() + timeoutMs;
  while (!pred() && Date.now() < end) await settle(25);
  return pred();
};

const results: { name: string; pass: boolean; detail?: string }[] = [];
function check(name: string, assertion: () => boolean) {
  let pass = false;
  let detail: string | undefined;
  try {
    pass = Boolean(assertion());
  } catch (error) {
    detail = String(error);
  }
  results.push(detail ? { name, pass, detail } : { name, pass });
}

function findOverviewConflictsButton(): HTMLButtonElement | undefined {
  const overview = stage.querySelector(".work-overview");
  if (!overview) return undefined;
  return [...overview.querySelectorAll("button")].find((btn) =>
    /conflict|resolve/i.test(btn.textContent?.trim() ?? ""),
  );
}

function findHideConflictsButton(): HTMLButtonElement | undefined {
  const bar = stage.querySelector('[data-testid="work-conflicts-bar"]');
  const section = stage.querySelector("#work-conflicts");
  const candidates = [
    ...(bar ? [...bar.querySelectorAll("button")] : []),
    ...(section ? [...section.querySelectorAll("button")] : []),
  ];
  return candidates.find((btn) =>
    /^(hide|close)$/i.test(btn.textContent?.trim() ?? "") ||
    /hide|close/i.test(btn.getAttribute("aria-label") ?? ""),
  );
}

const dataSummary: Record<string, unknown> = { scenario, width, theme };
let pane: Record<string, unknown> | undefined;

try {
  conflictSessions.clear();
  if (scenario === "drafts") {
    const draftSnap = makeRepoFixtures("conflicted", REPO).conflictDocs["src/conflict.ts"] as Parameters<typeof conflictSessions.open>[1];
    const opened = conflictSessions.open(REPO, draftSnap);
    conflictSessions.edit(REPO, "src/conflict.ts", {
      ...opened.state,
      choices: ["AcceptOurs"],
    });
  }

  await repoStore.openRepo(REPO);
  repoStore.setActiveTab("work", "overview");
  pane = mount(WorkOverviewPane, {
    target: stage,
    props: { loadConflict: () => Promise.resolve({ default: ConflictEditor }) },
  });

  await until(() => Boolean(stage.querySelector(".work-overview")), "WorkView did not mount");
  await settle(60);

  const initialWorkView = stage.querySelector(".work-overview");
  const layoutBefore =
    stage.querySelector("[data-overview-layout]")?.getAttribute("data-overview-layout") ?? null;
  dataSummary.layoutBefore = layoutBefore;

  check("WorkView mounts inside WorkOverviewPane", () => Boolean(initialWorkView));
  check("Overview fits viewport width without horizontal overflow", () =>
    stage.scrollWidth <= stage.clientWidth + 1,
  );

  if (scenario === "conflicted") {
    const arrived = await until(() => Boolean(stage.querySelector("#work-conflicts .resolve-page")), "ConflictEditor");
    check("conflicted repo leads Overview with #work-conflicts", () =>
      arrived &&
      stage.querySelector("[data-overview-layout]")?.getAttribute("data-overview-layout") === "with-conflicts",
    );
    check("WorkView DOM node identity preserved in conflicted layout", () =>
      stage.querySelector(".work-overview") === initialWorkView,
    );
  } else {
    const conflictsBtn = findOverviewConflictsButton();
    dataSummary.hasConflictsButton = Boolean(conflictsBtn);
    dataSummary.conflictsButtonText = conflictsBtn?.textContent?.trim() ?? null;

    check("Overview exposes a visible Conflicts/Resolve control even when not conflicted", () =>
      Boolean(conflictsBtn && conflictsBtn.getBoundingClientRect().width > 0),
    );

    if (conflictsBtn) {
      conflictsBtn.click();
    } else {
      openConflicts();
    }
    await settle(60);
    const openedSection = await until(
      () => Boolean(stage.querySelector("#work-conflicts .resolve-page")),
      "#work-conflicts",
      1500,
    );
    dataSummary.layoutAfterOpen =
      stage.querySelector("[data-overview-layout]")?.getAttribute("data-overview-layout") ?? null;
    dataSummary.activeElementId = document.activeElement?.id ?? null;

    check("opening Conflicts/Resolve reveals #work-conflicts and ConflictEditor", () =>
      openedSection && Boolean(stage.querySelector("#work-conflicts")),
    );
    check("opening #work-conflicts focuses section#work-conflicts", () =>
      document.activeElement?.id === "work-conflicts",
    );
    check("WorkView DOM node identity is preserved when opening Conflicts", () =>
      stage.querySelector(".work-overview") === initialWorkView,
    );

    const hideBtn = findHideConflictsButton();
    dataSummary.hasHideButton = Boolean(hideBtn);
    if (hideBtn) {
      hideBtn.click();
      await until(() => !stage.querySelector("#work-conflicts"), "close #work-conflicts", 1500);
    }
    dataSummary.layoutAfterClose =
      stage.querySelector("[data-overview-layout]")?.getAttribute("data-overview-layout") ?? null;

    check("closing clean #work-conflicts returns to overview layout without unmounting WorkView", () =>
      Boolean(hideBtn) &&
      !stage.querySelector("#work-conflicts") &&
      stage.querySelector(".work-overview") === initialWorkView,
    );

    if (scenario === "transition") {
      activeMode = "conflicted";
      await repoStore.refresh();
      const liveArrived = await until(
        () => Boolean(stage.querySelector("#work-conflicts .resolve-page")),
        "live conflict arrival",
        3000,
      );
      check("live conflict transition reveals #work-conflicts without remounting WorkView", () =>
        liveArrived && stage.querySelector(".work-overview") === initialWorkView,
      );

      activeMode = "clean";
      await repoStore.refresh();
      const cleared = await until(() => !stage.querySelector("#work-conflicts"), "live conflict cleared", 3000);
      check("resolving live conflict restores overview without remounting WorkView", () =>
        cleared && stage.querySelector(".work-overview") === initialWorkView,
      );
    }
  }

  dataSummary.workViewPreserved = stage.querySelector(".work-overview") === initialWorkView;
  dataSummary.overflowX = stage.scrollWidth > stage.clientWidth + 1;

  check("no uncaught runtime errors", () => crashes.length === 0);
  check("no unexpected Tauri IPC calls", () => unexpected.length === 0);
} catch (error) {
  crashes.push(String(error));
  results.push({ name: "harness execution", pass: false, detail: String(error) });
} finally {
  conflictSessions.clear();
  if (pane) {
    try {
      await unmount(pane);
    } catch {
      // Ignore cleanup unmount errors.
    }
  }
}

const payload = {
  results,
  passed: results.filter((r) => r.pass).length,
  total: results.length,
  crashes,
  unexpected,
  errors: crashes,
  data: dataSummary,
  diagnostics: JSON.stringify(dataSummary),
};

document.getElementById("verdict")!.textContent =
  results.map((r) => `${r.pass ? "PASS" : "FAIL"} ${r.name}`).join("\n");
document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify(payload)));
const report = params.get("report");
if (report?.startsWith("/__gp_result/")) {
  await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}
