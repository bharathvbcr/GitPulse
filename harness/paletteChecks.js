import "../src/app.css";
import { mount, tick } from "svelte";
import { get } from "svelte/store";
import { mockIPC } from "@tauri-apps/api/mocks";
import { repoStore } from "../src/lib/stores/repoStore";
import { themeStore } from "../src/lib/stores/themeStore";
import CommandPalette from "../src/lib/components/CommandPalette.svelte";
import { FRECENCY_KEY } from "../src/lib/palette/model";
import { graphStore } from "../src/lib/stores/graphStore";
import PromptModal from "../src/lib/components/PromptModal.svelte";
import { cancelPrompt } from "../src/lib/stores/modalStore";

const params = new URLSearchParams(location.search);
const results = [], crashes = [], calls = [], pending = new Map();
const branch = (name, current = false) => ({ name, is_current: current, is_remote: false, tip_commit_id: "abc123", ahead_count: 0, behind_count: 0, is_default: current, is_gone: false, last_commit_timestamp: 0, last_author: "Ada", last_summary: "Improve search", commits_ahead_of_base: 0, commits_behind_base: 0, additions: 0, deletions: 0, files_changed: 0 });
const symbol = (name, repo) => ({ symbol_name: name, file_path: `src/${name}.ts`, kind: "Function", span_start_line: 12, span_end_line: 18, source_span: "", score: 1, ...(repo ? { repo } : {}) });
const response = items => ({ available: true, items, shown: items.length, total: items.length, truncated: false });
let failWorkspace = false, missingRegistry = false, partialWorkspace = false, failedRepo = false;
let files = ["src/App.svelte", "src/lib/components/CommandPalette.svelte", "README.md"];
const fixture = {
 cmd_resolve_repo: a => { if(failedRepo) throw Error("Repository missing"); return { path: a.path, name: "GitPulse", is_bare: false }; },
 cmd_list_branches: () => [branch("main", true), branch("feature/palette")],
 cmd_get_status: () => [], cmd_list_tags: () => ({ tags: [], truncated: false }), cmd_stash_list: () => [],
 cmd_watch_repo: () => null, cmd_unwatch_repo: () => null, cmd_set_recent_menu: () => null,
 cmd_repo_operation: () => null,
 cmd_branch_stats: () => ({ updates: [], capped: false, compute_failures: 0, compared_to: "main" }),
 cmd_get_commit_graph: () => ({ rows: [], refs: [], head_id: null, has_more: false }),
 cmd_workspace_sync: () => ({ repos: [] }),
 cmd_workspace_list: () => ({ version: 1, registry_root: "/fixture/GitPulse", registry_path: "fixture", repos: missingRegistry ? [] : [{ name: "other", root: "/fixture/other", db: "", db_path: "" }] }),
 cmd_list_repo_files: () => files,
 cmd_codeintel_search: a => new Promise(resolve => pending.set(a.query, resolve)),
 cmd_workspace_search: () => { if (failWorkspace) throw Error("Workspace offline"); return { items: [symbol("workspaceResult", "other")], shown: 1, total: partialWorkspace ? 75 : 1, hidden: partialWorkspace ? 74 : 0, truncated: partialWorkspace, unavailable: partialWorkspace ? [{repo:"unavailable-repo",reason:"Index missing"}] : [], repos_queried: 1, semantic: false }; },
};
mockIPC((cmd, args) => { calls.push({ cmd, args }); if (fixture[cmd]) return fixture[cmd](args); throw Error(`Unconfigured command: ${cmd}`); }, { shouldMockEvents: true });
themeStore.setTheme(params.get("theme") === "light" ? "light" : "dark");
await repoStore.openRepo("/fixture/GitPulse");
const opener = document.getElementById("opener");
const root = document.getElementById("app");
opener.onclick = () => window.dispatchEvent(new CustomEvent("gitpulse:palette"));
mount(CommandPalette, { target: root });
const promptRoot = document.createElement("div"); document.body.append(promptRoot);
mount(PromptModal, { target: promptRoot });
const settle = async () => { await tick(); await new Promise(resolve => setTimeout(resolve, 30)); await tick(); };
const waitFor = async predicate => { for (let i=0;i<150;i++) { if(predicate()) return; await settle(); } throw Error("Timed out waiting for palette state"); };
const input = () => root.querySelector("input");
const check = (name, pass) => results.push({ name, pass: Boolean(pass) });
const type = async value => { input().value = value; input().dispatchEvent(new Event("input", { bubbles: true })); await settle(); };
const open = async () => { opener.focus(); opener.click(); await settle(); };
const key = async (value, opts = {}) => { (document.activeElement ?? window).dispatchEvent(new KeyboardEvent("keydown", { key: value, bubbles: true, cancelable: true, ...opts })); await settle(); };
const option = text => [...root.querySelectorAll("[role=option]")].find(el => el.textContent.includes(text));
window.addEventListener("error", e => crashes.push(e.message));
window.addEventListener("unhandledrejection", e => crashes.push(String(e.reason)));
await settle();
await open();
if (params.has("check")) {
 try {
  check("palette opens focused", document.activeElement === input());
  await type("?"); option("Type #").click(); await settle();
  check("help changes mode without closing", Boolean(input()) && input().value === "#");
  await open(); await type(":older"); await waitFor(() => pending.has("older"));
  await type(":newer"); await waitFor(() => pending.has("newer"));
  pending.get("newer")(response([symbol("newer")])); await waitFor(() => option("newer"));
  pending.get("older")(response([symbol("older")])); await settle();
  check("older symbol responses cannot replace current results", Boolean(option("newer")) && !option("older"));
  failWorkspace = true; await type("::offline"); await settle(); await new Promise(resolve => setTimeout(resolve, 300)); await settle();
  check("workspace failure is visible", /failed|offline/i.test(root.textContent));
  failWorkspace = false; await type("::working"); await waitFor(() => option("workspaceResult")); option("workspaceResult").click(); await settle();
  check("workspace hit opens the registered repository", get(repoStore).currentPath === "/fixture/other");
  check("workspace hit selects its file", get(repoStore).selectedFilePath === "src/workspaceResult.ts" && get(repoStore).viewSections.code === "explorer");
  await open(); await key("Escape"); check("Escape restores the opener's focus", document.activeElement === opener && !input());
  await key("k", { metaKey: true }); check("keyboard shortcut opens after first mount", Boolean(input()));
  await key("k", { metaKey: true }); check("keyboard shortcut toggles closed exactly once", !input());
  await open(); await type("Open Settings"); await key("Enter", { isComposing: true });
  check("IME Enter cannot execute a command", Boolean(input()));
  await type("@"); check("current branch is visibly unavailable", option("main")?.getAttribute("aria-disabled") === "true");
  option("main").click(); await settle(); check("disabled rows keep the palette open", Boolean(input()) && !calls.some(c => c.cmd === "cmd_checkout_branch"));
  await type("qckcmt"); check("fuzzy aliases find Quick Commit", Boolean(option("Quick Commit")));
  check("clean-tree Quick Commit explains why it cannot run", option("Quick Commit")?.textContent.includes("Nothing to commit"));
  files = Array.from({ length: 125 }, (_, i) => `src/file-${String(i).padStart(3,"0")}.ts`);
  await type("/"); await waitFor(() => option("file-000.ts"));
  check("large file lists render only the first page", root.querySelectorAll("[role=option]").length === 50 && root.textContent.includes("1–50 of 125 results"));
  await key("PageDown"); check("PageDown reaches the next page", root.textContent.includes("51–100 of 125 results") && Boolean(option("file-050.ts")));
  await key("End"); check("End highlights the final visible option", input().getAttribute("aria-activedescendant") === "palette-option-49");
  await key("PageDown"); check("last result page remains reachable", Boolean(option("file-124.ts")) && root.querySelectorAll("[role=option]").length === 25);
  await type("/file-124.ts"); check("filtering searches the entire file inventory", Boolean(option("file-124.ts")) && root.textContent.includes("1–1 of 1 results"));
  check("file typing reuses the current inventory", calls.filter(c => c.cmd === "cmd_list_repo_files").length === 1);
  await type("/no-such-file"); check("empty results never reference a missing active descendant", !input().hasAttribute("aria-activedescendant") && root.textContent.includes("No matching results"));
  await type("/file-124.ts"); await key("Enter"); check("file results open Explorer with the right path", !input() && get(repoStore).selectedFilePath === "src/file-124.ts");
  await open(); await type(":closing"); await key("Escape"); await new Promise(resolve => setTimeout(resolve, 250));
  check("closing cancels a queued search", !pending.has("closing"));
  await open(); await type(":switch-mode"); await waitFor(() => pending.has("switch-mode")); await type("Open Settings");
  pending.get("switch-mode")(response([symbol("stale-after-mode-switch")])); await settle();
  check("a mode change invalidates in-flight results", Boolean(option("Open Settings")) && !option("stale-after-mode-switch"));
  await type(":switch-repo"); await waitFor(() => pending.has("switch-repo"));
  const oldResolve = pending.get("switch-repo"); await repoStore.openRepo("/fixture/different"); await settle();
  oldResolve(response([symbol("stale-repo-result")])); await settle();
  check("repository changes invalidate prior symbol results", !option("stale-repo-result"));
  await type("::missing"); missingRegistry = true; await waitFor(() => option("workspaceResult"));
  check("unresolvable workspace hits are disabled", option("workspaceResult").getAttribute("aria-disabled") === "true");
  option("workspaceResult").click(); await settle(); check("unresolvable hits cannot change the active file", get(repoStore).currentPath === "/fixture/different" && get(repoStore).selectedFilePath !== "src/workspaceResult.ts");
  missingRegistry = false; partialWorkspace = true; await type("::partial"); await waitFor(() => root.textContent.includes("1 of 75"));
  check("partial workspace results retain coverage and failure notices", Boolean(option("workspaceResult")) && root.textContent.includes("unavailable-repo: Index missing"));
  partialWorkspace = false; failedRepo = true; option("workspaceResult").click(); await waitFor(() => root.querySelector("[role=alert]"));
  check("failed workspace activation leaves recovery in the palette", Boolean(input()) && get(repoStore).currentPath === "/fixture/different"); failedRepo = false;
  await type("Refresh Repository Status");
  const realRefresh = repoStore.refresh; let finishRefresh; let refreshCalls = 0;
  repoStore.refresh = () => { refreshCalls++; return new Promise(resolve => { finishRefresh = resolve; }); };
  await key("Enter"); await key("Enter"); check("repeated Enter starts only one action", refreshCalls === 1 && root.textContent.includes("Action in progress"));
  finishRefresh({ok:false,error:"Fixture action refused"}); await settle();
  check("refused actions remain visible with an error", root.querySelector("[role=alert]")?.textContent.includes("Fixture action refused") && Boolean(input()));
  repoStore.refresh = realRefresh;
  await type("Create New Branch"); await key("Enter"); await waitFor(() => promptRoot.querySelector("input"));
  check("follow-on prompts receive focus after the palette closes", !input() && document.activeElement === promptRoot.querySelector("input"));
  await key("k", { metaKey: true }); check("the palette cannot cover an active prompt", !input());
  cancelPrompt(); await new Promise(resolve => setTimeout(resolve, 100)); await open();
  check("cancelled prompts do not record command use", !Object.hasOwn(JSON.parse(localStorage.getItem(FRECENCY_KEY) ?? "{}"), "new_branch"));
  await type("?"); check("help is filtered by its search text", Boolean(option("Type #")));
  await type("?workspace"); check("help search narrows the mode list", Boolean(option("Type ::")) && !option("Type #"));
  await key("Escape"); localStorage.setItem(FRECENCY_KEY,"null"); await open(); check("malformed history cannot crash reopening", Boolean(input()));
  await type("@"); await key("Home"); check("Home highlights the first result", input().getAttribute("aria-activedescendant") === "palette-option-0");
  await key("Tab"); check("Tab stays inside the palette", root.contains(document.activeElement));
  await type("#"); graphStore.showRepo("/fixture/not-current"); await settle();
  check("commit search never uses a different repository's history", root.querySelectorAll("[role=option]").length === 0 && root.textContent.includes("History has not loaded"));
  await key("Escape");
  while(get(repoStore).openTabs.length) await repoStore.closeActiveTab();
  await open(); await type("Fetch All Remotes"); check("repository actions explain the no-repository state", option("Fetch All Remotes")?.getAttribute("aria-disabled") === "true" && root.textContent.includes("Open a repository first"));
  await type("fleet"); check("workspace Fleet remains available without a repository", option("Open Fleet")?.getAttribute("aria-disabled") === "false");
  await type("/"); check("file search offers repository recovery", root.textContent.includes("Choose a repository"));
  check("no uncaught runtime errors", crashes.length === 0);
 } catch (error) { check(String(error), false); }
 const result = { results, passed: results.filter(r => r.pass).length, total: results.length, crashes };
 document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify(result)));
 document.getElementById("verdict").textContent = `${result.passed}/${result.total} checks passed`;
 if (params.has("report")) await fetch(params.get("report"), { method: "POST", body: document.documentElement.outerHTML });
}
