import "../../../src/app.css";
import { mount, tick, unmount } from "svelte";
import { get } from "svelte/store";
import { mockIPCWithEvents } from "../../../harness/tauriMocks";
import { repoStore } from "../../../src/lib/stores/repoStore";
import { interfaceStore } from "../../../src/lib/stores/interfaceStore";
import { noteForegroundFocus } from "../../../src/lib/runtime/foreground";
import { terminalSessions } from "../../../src/lib/terminal/sessionRegistry";
import { sessionActivity } from "../../../src/lib/terminal/sessionActivity";
import AgentsView from "../../../src/lib/components/AgentsView.svelte";
import { makeInsightsSnapshot } from "../helpers/fixtures";

const params = new URLSearchParams(location.search);
const scenario = params.get("scenario") ?? "mixed";
const width = Number(params.get("width") ?? (scenario === "narrow" ? "390" : "1280"));
const theme = params.get("theme") === "light" ? "light" : "dark";
document.documentElement.classList.toggle("light", theme === "light");
document.documentElement.classList.toggle("dark", theme === "dark");

const stage = document.getElementById("stage")!;
stage.style.width = `${width}px`;

const crashes: string[] = [];
const unexpected: string[] = [];
window.addEventListener("error", (event) => crashes.push(event.message));
window.addEventListener("unhandledrejection", (event) => crashes.push(String(event.reason)));

const MAIN = "/code/devtools/GitPulse";
const WT_CLAUDE_BLOCK = `${MAIN}/.claude/worktrees/claude-block`;
const WT_AGY_INFO = `${MAIN}/.claude/worktrees/agy-info`;
const WT_CLAUDE_RUN = `${MAIN}/.claude/worktrees/claude-run`;

const worktrees = [
  {
    path: MAIN,
    name: "GitPulse",
    branch: "main",
    is_detached: false,
    is_main: true,
    is_bare: false,
    dirty_files: 0,
    agent_kind: "",
    session_slug: "",
    operation_kind: "",
    operation_ok: true,
  },
  {
    path: WT_CLAUDE_BLOCK,
    name: "claude-block",
    branch: "agent/claude-block",
    is_detached: false,
    is_main: false,
    is_bare: false,
    dirty_files: 2,
    agent_kind: "claude",
    session_slug: "claude-block",
    operation_kind: "",
    operation_ok: true,
  },
  {
    path: WT_AGY_INFO,
    name: "agy-info",
    branch: "agent/agy-info",
    is_detached: false,
    is_main: false,
    is_bare: false,
    dirty_files: 0,
    agent_kind: "agy",
    session_slug: "agy-info",
    operation_kind: "",
    operation_ok: true,
  },
  {
    path: WT_CLAUDE_RUN,
    name: "claude-run",
    branch: "agent/claude-run",
    is_detached: false,
    is_main: false,
    is_bare: false,
    dirty_files: 1,
    agent_kind: "claude",
    session_slug: "claude-run",
    operation_kind: "",
    operation_ok: true,
  },
];

mockIPCWithEvents((cmd, args) => {
  if (cmd.startsWith("plugin:")) return null;
  const payload = (args ?? {}) as Record<string, unknown>;
  if (cmd === "cmd_resolve_repo") {
    return { path: String(payload.repoPath ?? MAIN), name: "GitPulse", is_bare: false, common_dir: `${MAIN}/.git` };
  }
  if (cmd === "cmd_list_branches") {
    return [{
      name: "main",
      is_current: true,
      is_remote: false,
      remote_name: null,
      tip_commit_id: "abc123456789",
      is_default: true,
      is_gone: false,
      last_commit_timestamp: 1700000000,
      last_author: "Alex",
      last_summary: "Work",
    }];
  }
  if (cmd === "cmd_get_status") return [];
  if (cmd === "cmd_list_tags") return { tags: [], truncated: false };
  if (cmd === "cmd_repo_operation") return null;
  if (cmd === "cmd_stash_list") return { entries: [], truncated: false };
  if (cmd === "cmd_branch_stats") return { updates: [], capped: false, compute_failures: 0, compared_to: "main" };
  if (cmd === "cmd_watch_repo" || cmd === "cmd_unwatch_repo" || cmd === "cmd_set_recent_menu") return null;
  if (cmd === "cmd_last_fetch_at" || cmd === "cmd_note_tab_activated") return null;
  if (cmd === "cmd_get_commit_graph") return { nodes: [], edges: [], total_commits: 0, truncated: false };
  if (cmd === "cmd_workspace_sync") return { repos: [] };
  if (cmd === "cmd_devcouncil_init") {
    return {
      repo: String(payload.repoPath ?? MAIN),
      state_dir: `${MAIN}/.devmap`,
      exclude: { status: "already_ignored", source: "harness" },
      workspace_registry: null,
      workspace_reason: null,
      skipped_untrusted: [],
      skipped_unavailable: [],
      devmap_available: false,
    };
  }
  if (cmd === "cmd_devmap_maybe_refresh") {
    return {
      decision: "skip_unavailable",
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: null,
    };
  }
  if (cmd === "cmd_insights_snapshot") return makeInsightsSnapshot(MAIN, worktrees);
  if (cmd === "cmd_workbench_request") {
    const method = String(payload.method ?? "");
    if (method === "repositories.list") {
      return JSON.stringify({
        ok: true,
        items: [{ id: "repo-1", revision: 1, updated_at: 1, name: "GitPulse", identity_key: `local:${MAIN}/.git`, remote_url: null }],
        total: 1,
        shown: 1,
        has_more: false,
        next_cursor: null,
      });
    }
    if (method === "runs.list") {
      return JSON.stringify({ ok: true, items: [], total: 0, shown: 0, has_more: false, next_cursor: null });
    }
  }
  if (cmd === "cmd_inbox_read") {
    return { ok: true, repo_path: MAIN, threads: [], lanes: [], total: 0 };
  }
  if (cmd === "cmd_terminal_context") {
    const sid = String(payload.sessionId ?? "");
    if (sid === "pty-claude-block") {
      return { process: "claude", busy: false, cwd: WT_CLAUDE_BLOCK, repo_dir: ".claude/worktrees/claude-block" };
    }
    if (sid === "pty-agy-info") {
      return { process: "agy", busy: false, cwd: WT_AGY_INFO, repo_dir: ".claude/worktrees/agy-info" };
    }
    if (sid === "pty-claude-run") {
      return { process: "claude", busy: true, cwd: WT_CLAUDE_RUN, repo_dir: ".claude/worktrees/claude-run" };
    }
    return { process: null, busy: null, cwd: null, repo_dir: null };
  }
  if (cmd === "cmd_session_attention_answered") return null;
  unexpected.push(cmd);
  return null;
});

const settle = async (ms = 40) => {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, ms));
  await tick();
};

const until = async (pred: () => boolean, label: string, timeoutMs = 5000) => {
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

const dataSummary: Record<string, unknown> = { scenario, width, theme };
let view: Record<string, unknown> | undefined;
const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
let revealed = 0;

try {
  noteForegroundFocus(true);
  await repoStore.openRepo(MAIN);
  interfaceStore.setAgentsOpen(true);

  slots.push(
    terminalSessions.reserve({
      key: "live-claude-block",
      repoPath: MAIN,
      label: "Claude Code",
      launcher: "claude",
      title: "claude-block",
      status: "running",
      sessionId: "pty-claude-block",
      close: async () => {},
      reveal: () => {
        revealed += 1;
      },
    }),
  );
  slots.push(
    terminalSessions.reserve({
      key: "live-agy-info",
      repoPath: MAIN,
      label: "Antigravity",
      launcher: "agy",
      title: "agy-info",
      status: "running",
      sessionId: "pty-agy-info",
      close: async () => {},
      reveal: () => {
        revealed += 1;
      },
    }),
  );
  slots.push(
    terminalSessions.reserve({
      key: "live-claude-run",
      repoPath: MAIN,
      label: "Claude Code",
      launcher: "claude",
      title: "claude-run",
      status: "running",
      sessionId: "pty-claude-run",
      close: async () => {},
      reveal: () => {},
    }),
  );

  sessionActivity.announce({
    session: "pty-claude-block",
    channel: "hook",
    event: "permission_prompt",
    detail: "Allow Bash: git push --dry-run",
  });

  sessionActivity.announce({
    session: "pty-agy-info",
    channel: "hook",
    event: scenario === "claude-vs-agy" ? "turn_finished" : "agent_completed",
    detail: "Completed plan synthesis cleanly",
  });

  view = mount(AgentsView, { target: stage });
  const root = () => stage.querySelector<HTMLElement>("[data-testid=agents-view]");
  const row = (name: string) =>
    [...(root()?.querySelectorAll("[data-testid=agents-row]") ?? [])].find((n) =>
      n.textContent?.includes(name),
    );
  const filterBtn = (labelRe: RegExp) =>
    [...(root()?.querySelectorAll("[data-testid^=agents-filter-]") ?? [])].find((b) =>
      labelRe.test(b.textContent?.trim() ?? ""),
    ) as HTMLButtonElement | undefined;

  await until(() => Boolean(row("claude-block") && row("agy-info")), "agent rows did not mount");
  await settle(60);

  const claudeRow = row("claude-block");
  const agyRow = row("agy-info");
  const runRow = row("claude-run");

  dataSummary.rowTexts = {
    claudeBlock: claudeRow?.textContent?.trim() ?? "",
    agyInfo: agyRow?.textContent?.trim() ?? "",
    claudeRun: runRow?.textContent?.trim() ?? "",
  };
  dataSummary.filterLabels = [...(root()?.querySelectorAll("[data-testid^=agents-filter-]") ?? [])].map(
    (b) => b.textContent?.trim(),
  );

  check("AgentsView renders labelled region and treegrid", () =>
    root()?.getAttribute("aria-label") === "Agents" &&
    root()?.querySelector("table")?.getAttribute("role") === "treegrid",
  );

  check("rows render LauncherIcon SVGs for Claude and Antigravity", () =>
    Boolean(claudeRow?.querySelector("svg") && agyRow?.querySelector("svg")),
  );

  check("attention cells use semantic gp-chip badges with data-tone", () =>
    Boolean(
      claudeRow?.querySelector(".gp-chip[data-tone]") &&
        agyRow?.querySelector(".gp-chip[data-tone]"),
    ),
  );

  check("inline detail previews are rendered in the table", () =>
    Boolean(
      claudeRow?.textContent?.includes("Allow Bash: git push --dry-run") &&
        agyRow?.textContent?.includes("Completed plan synthesis cleanly"),
    ),
  );

  const needsActionBtn = filterBtn(/needs action/i);
  if (needsActionBtn) {
    needsActionBtn.click();
    await settle(40);
  }
  check("Needs action filter keeps blocking Claude row and excludes informational Antigravity row", () =>
    Boolean(needsActionBtn && row("claude-block") && !row("agy-info")),
  );

  const notificationsBtn = filterBtn(/notifications?|notices?/i);
  dataSummary.hasNotificationsFilter = Boolean(notificationsBtn);
  if (notificationsBtn) {
    notificationsBtn.click();
    await settle(40);
    check("Notifications filter shows informational Antigravity notice without blocking row", () =>
      Boolean(row("agy-info") && !row("claude-block")),
    );
  } else {
    check("Notifications filter is exposed in AgentsView toolbar", () => false);
  }

  const allBtn = filterBtn(/^all/i);
  if (allBtn) {
    allBtn.click();
    await settle(40);
  }

  if (scenario === "dismiss") {
    const dismissBtn = [...(row("agy-info")?.querySelectorAll("button") ?? [])].find((b) =>
      /dismiss|acknowledge|clear/i.test(b.textContent?.trim() ?? b.getAttribute("aria-label") ?? ""),
    ) as HTMLButtonElement | undefined;
    dataSummary.hasDismissButton = Boolean(dismissBtn);
    if (dismissBtn) {
      dismissBtn.click();
      await settle(40);
    }
    check("dismiss action clears informational notification while keeping live row", () =>
      Boolean(dismissBtn && row("agy-info") && !row("agy-info")?.textContent?.includes("Completed plan synthesis cleanly")),
    );
  }

  const showBtn = row("claude-block")?.querySelector<HTMLButtonElement>(
    "[aria-label='Show terminal for claude-block']",
  );
  if (showBtn) {
    showBtn.click();
    await settle(40);
  }
  check("Focus / Show terminal action focuses the terminal session", () =>
    revealed === 1 && get(interfaceStore).globalSurface === "repository",
  );
  interfaceStore.setAgentsOpen(true);
  await settle(40);

  if (scenario === "narrow" || width <= 420) {
    dataSummary.overflowX = stage.scrollWidth > stage.clientWidth + 1;
    check("narrow viewport fits without horizontal overflow", () =>
      stage.scrollWidth <= stage.clientWidth + 1 &&
      (root()?.scrollWidth ?? 0) <= (root()?.clientWidth ?? 0) + 1,
    );
  }

  check("no uncaught runtime errors", () => crashes.length === 0);
  check("no unexpected Tauri IPC calls", () => unexpected.length === 0);
} catch (error) {
  crashes.push(String(error));
  results.push({ name: "harness execution", pass: false, detail: String(error) });
} finally {
  for (const slot of slots) {
    try {
      slot.release();
    } catch {
      // Ignore cleanup errors.
    }
  }
  if (view) {
    try {
      await unmount(view);
    } catch {
      // Ignore cleanup errors.
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
