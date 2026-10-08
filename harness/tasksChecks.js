import "../src/app.css";
import { mount, tick } from "svelte";
import { get } from "svelte/store";
import { promptState } from "../src/lib/stores/modalStore";
import { mockIPCWithEvents } from "./tauriMocks";
import { applyPlatformClass } from "../src/lib/platform";
import { shortcutTextLabel } from "../src/lib/ui/platformCopy";
import { hostPlatform } from "../src/lib/stores/platformStore";
import TasksHost from "./TasksHost.svelte";
import { requestTaskOpen, taskOpenRequest } from "../src/lib/workbench/taskOpen";
import { themeStore } from "../src/lib/stores/themeStore";
import { harnessStore } from "../src/lib/stores/harnessStore";
import { repoStore } from "../src/lib/stores/repoStore";
import { interfaceStore } from "../src/lib/stores/interfaceStore";
import { describeForeground, holdForeground } from "./foreground";

const params = new URLSearchParams(location.search);
const results = [], crashes = [], writes = [], unknown = [];
// Attempts holding a checkout and their requests, for the board's agent
// marks. Empty until the checks for those marks fill them.
let boardRuns = [], boardDecisions = [], boardRunsFail = false;
const copies = [];
Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (text) => { copies.push(text); } } });
const preparedRuns = [], appleDrafts = [];
// Off, the store refuses every preparation. On, it accepts one and starts a
// managed attempt, which is the only way a handoff's success path — the host
// closing the sheet from inside the form's own launch — is exercised.
let acceptPreparation = false;
// The code a refused preparation carries. `store_error` leaves the form's
// request pending for an exact retry; a definite refusal clears it.
let prepareRefusal = "store_error";
const fixtureRuns = new Map();
const issueCalls = [];
// `issueFailOnCall` aims a failure at one call of a run (1-based over every
// call so far); 0 fails them all. `holdIssue` parks a creation mid-run.
let issueFailure = "", issueFailOnCall = 0, holdIssue = false, releaseIssue, failIssueLinkOnce = false;
let appleStatus = { compiled: true, state: "available", reason: null, detail: "The on-device model is ready. Nothing leaves this Mac." };
let appleFailure = "";
const repos = ["GitPulse", "Manvi"].map((name, i) => ({ id: `repo-${i}`, name, revision: 1, updated_at: 1, identity_key: `local:/fixture/${name}/.git`, remote_url: null }));
const workspace = { id: "workspace", name: "Developer tools", revision: 1, updated_at: 1, icon: "", color: "", pinned: false, archived: false, position: 1, description: "", repository_ids: ["repo-1"], repository_count: 1 };
// A workspace with no member repositories. The board used to refuse New task
// here with nothing on screen saying why, and the empty state offered a New
// task that opened a sheet which could never be saved.
const emptyWorkspace = { id: "workspace-empty", name: "Fresh space", revision: 1, updated_at: 1, icon: "", color: "", pinned: false, archived: false, position: 2, description: "", repository_ids: [], repository_count: 0 };
const workspaces = [workspace, emptyWorkspace];
const workspaceWrites = [];
const makeTask = (id, title, repository, status = "ready") => ({ id, title, kind: "feature", status, priority: 2, severity: null, owner: null, due_at: null, labels: [], repository_ids: [repository], primary_repository_id: repository, home_workspace_id: null, position: Number(id.replace(/\D/g, "")) || 1, revision: 1, updated_at: 1, description: "Keep changes focused and verify the result.", acceptance_criteria: [], locked_fields: [], checklist: [], links: [],
  // As dc-store schema 11's upgrade leaves a profile: work that was Done is
  // archived, with the time it was completed.
  archived: status === "done", completed_at: status === "done" ? 1_000 + (Number(id.replace(/\D/g, "")) || 0) : null });
/** Store-maintained, as dc-store keeps it: set entering Done, kept while there, cleared leaving it. */
const completedAt = (previous, status) => status !== "done" ? null : previous?.status === "done" ? previous.completed_at ?? null : Math.floor(Date.now() / 1000);
let tasks = [
  makeTask("task-1", "Keep repository tasks in sync", "repo-0", "in_progress"),
  { ...makeTask("task-2", "Make task sheets easier to scan", "repo-0", "review"), priority: 1, kind: "improvement", owner: "Bharath", labels: ["usability", "tasks", "desktop"], repository_ids: ["repo-0", "repo-1"] },
  makeTask("task-3", "Review the agent handoff", "repo-1", "backlog"),
  ...Array.from({length: 32}, (_, i) => makeTask(`task-${i + 10}`, `Ready task ${String(i + 1).padStart(2, "0")}`, "repo-0")),
  // More completed tasks than one page holds, so the archive's "showing N of
  // M" line and its Load more are exercised against a real second page rather
  // than a list that happens to fit.
  ...Array.from({length: 34}, (_, i) => makeTask(`task-${i + 100}`, `Completed task ${String(i + 1).padStart(2, "0")}`, "repo-0", "done")),
];
let corruptDelete = false, loseDelete = false;
const deleted = new Set(), deleteWrites = [], restoreWrites = [], mergeWrites = [];
let mergeFailSource = "";
let failList = false, corruptSave = false, loseSave = false, holdSave = false, releaseSave, holdSearch = false, heldSearch = [], holdGet = false, releaseGet;
const receipts = new Map(), proposals = new Map(), enhancementWrites = [], enhancementReads = [];
let failConfiguration = false, blankConfiguration = false, loseEnhancement = false, holdDelete = false, releaseDelete;
// Relinking a moved checkout and importing tab groups (see the end of the run).
let pickFolderResult = null, registerUnknown = false, loseRelink = false;
const relinkCalls = [], repoCommandCalls = [];
const missingCheckouts = new Set(), uncheckableCheckouts = new Set();
// What `repoStore.openRepo` asks the host for, answered the way an empty,
// healthy repository would. Only the tab-group checks open tabs.
const fixtureRepoCommands = {
  cmd_resolve_repo: a => ({ path: a.repoPath, name: a.repoPath.split("/").pop(), is_bare: false }),
  // A task terminal opens the checkout holding the attempt's directory; the
  // fixture's directories are checkout roots.
  // Board checkout checks go through here too: a folder in `missingCheckouts`
  // is refused the way the host refuses a folder with no repository, and one in
  // `uncheckableCheckouts` fails for a reason that says nothing about it.
  cmd_resolve_git_root: a => {
    if (missingCheckouts.has(a.path)) throw `Not a Git repository: ${a.path}`;
    if (uncheckableCheckouts.has(a.path)) throw `Operation not permitted (os error 1): ${a.path}`;
    return a.path;
  },
  cmd_watch_repo: () => null, cmd_unwatch_repo: () => null, cmd_set_recent_menu: () => null,
  cmd_list_branches: () => [], cmd_get_status: () => [], cmd_list_tags: () => ({ tags: [], truncated: false }),
  cmd_stash_list: () => ({ entries: [], truncated: false }),
  cmd_branch_stats: () => ({ updates: [], capped: false, compute_failures: 0, compared_to: "main" }),
  cmd_workspace_sync: () => ({ repos: [] }),
  cmd_repo_operation: () => null, cmd_last_fetch_at: () => null,
  cmd_devcouncil_init: a => ({ repo: a.repoPath, state_dir: `${a.repoPath}/.devmap`, exclude: { status: "already_ignored", source: "harness" }, workspace_registry: null, workspace_reason: null, skipped_untrusted: [], skipped_unavailable: [], devmap_available: false }),
  cmd_devmap_maybe_refresh: () => ({ decision: "skip_unavailable", facts: { available: false, is_fresh: false, schema_ok: false, already_building: false }, reason: null }),
};
const page = (items, start = 0, limit = 200) => ({ ok: true, items: items.slice(start, start + limit), total: items.length, shown: items.slice(start, start + limit).length, has_more: start + limit < items.length, next_cursor: start + limit < items.length ? String(start + limit) : null });
mockIPCWithEvents(async (cmd, args) => {
  if (cmd === "cmd_workbench_register_repository") {
    const known = repos.find(repo => repo.identity_key === `local:${args.repoPath}/.git`);
    if (known) return JSON.stringify({ repository: known });
    // A folder the store has not seen registers as a record of its own, as
    // the host does — which is how a moved checkout ends up as a second one.
    if (!registerUnknown) return JSON.stringify({ repository: undefined });
    const name = args.repoPath.split("/").pop();
    const created = { id: args.id, name, revision: 1, updated_at: 1, identity_key: `local:${args.repoPath}/.git`, remote_url: null };
    repos.push(created);
    return JSON.stringify({ repository: created });
  }
  if (cmd === "cmd_pick_folder") return pickFolderResult;
  // The host resolves the folder, then the store swaps the identity under the
  // same id: tasks keep pointing at it. Receipts make a retried request
  // return the first answer instead of relinking twice.
  if (cmd === "cmd_workbench_relink_repository") {
    relinkCalls.push(structuredClone(args));
    if (receipts.has(args.requestId)) return receipts.get(args.requestId);
    const index = repos.findIndex(repo => repo.id === args.repositoryId);
    if (index < 0) throw {code:"not_found", message:"Fixture has no such repository"};
    if (repos[index].revision !== args.expectedRevision) throw {code:"revision_conflict", message:"Repository changed"};
    const identity = `local:${args.repoPath}/.git`;
    const holder = repos.find(repo => repo.identity_key === identity && repo.id !== args.repositoryId);
    if (holder && tasks.some(task => task.repository_ids.includes(holder.id))) throw {code:"repository_not_empty", message:`the repository registered at that checkout holds task links; distinct records are never merged`};
    if (holder) repos.splice(repos.indexOf(holder), 1);
    const at = repos.findIndex(repo => repo.id === args.repositoryId);
    repos[at] = { ...repos[at], identity_key: identity, name: args.repoPath.split("/").pop(), revision: repos[at].revision + 1 };
    const result = JSON.stringify({ repository: repos[at], path: args.repoPath, is_bare: false });
    receipts.set(args.requestId, result);
    if (loseRelink) { loseRelink = false; throw {code:"transport_error", message:"Lost relink reply"}; }
    return result;
  }
  if (fixtureRepoCommands[cmd]) { repoCommandCalls.push(cmd); return fixtureRepoCommands[cmd](args); }
  if (cmd === "cmd_ai_status") {
    return {
      harness: { available: false, binary: "", protocol: 0, posture: "", ops: [], error: "", error_code: "" },
      endpoints: [{ base_url: "http://127.0.0.1:11434/v1", reachable: true, detail: "", models: ["quick-fixture"] }],
      selected: { base_url: "http://127.0.0.1:11434/v1", model: "quick-fixture" },
      model_info: null,
      model_detail: "",
      ready: true,
      detail: "Fixture model ready.",
    };
  }
  // Never reaches `gh`: filing an issue is published the moment it runs, so
  // the fixture records what the board sent and answers the way the guarded
  // command does — a policy verdict plus gh's printed URL.
  if (cmd === "cmd_github_create_issue") {
    issueCalls.push(structuredClone(args));
    if (holdIssue) await new Promise(resolve => { releaseIssue = resolve; });
    if (issueFailure && (!issueFailOnCall || issueCalls.length === issueFailOnCall)) throw issueFailure;
    return { policy: { status: "allow", checked: true, target: "gh issue create", rule: "", severity: "", reason: "", demoted: "", grant_id: "", grant_issuer: "" }, output: `Creating issue in fixture/GitPulse\n\nhttps://github.com/fixture/GitPulse/issues/${77 + issueCalls.length - 1}\n` };
  }
  if (cmd === "cmd_apple_intelligence_status") return appleStatus;
  if (cmd === "cmd_apple_intelligence_draft") {
    appleDrafts.push(structuredClone(args.request));
    if (appleFailure) throw { code: appleFailure, message: "Apple Intelligence declined." };
    const fields = args.request.fields;
    return {
      title: fields.includes("title") ? "Route notifications to the right task" : null,
      description: fields.includes("description") ? "Keep the saved evidence and explain recovery." : null,
      rationale: "Written on this Mac by Apple Intelligence. No text left the device.",
    };
  }
  if (cmd !== "cmd_workbench_request") { unknown.push(cmd); throw Error(`Unconfigured command: ${cmd}`); }
  const input = JSON.parse(args.input);
  switch (args.method) {
    case "repositories.list": return JSON.stringify(page(repos));
    case "repositories.get": return JSON.stringify({ok:true, item: repos.find(repo => repo.id === input.id)});
    case "runs.list": {
      if (boardRunsFail) throw {code:"store_error", message:"Fixture runs are unavailable"};
      const items = boardRuns.filter(run => (!input.state || run.state === input.state) && (!input.task_id || run.task_id === input.task_id) && (!input.repository_id || run.repository_id === input.repository_id));
      return JSON.stringify({ok:true, items, shown:items.length, total:items.length, has_more:false, next_cursor:null});
    }
    case "decisions.list": {
      const items = boardDecisions.filter(item => item.run_id === input.run_id && (!input.state || item.state === input.state));
      return JSON.stringify({ok:true, items, shown:items.length, total:items.length, has_more:false, next_cursor:null});
    }
    case "runs.prepare_terminal": case "runs.prepare_managed": {
      preparedRuns.push(structuredClone(input));
      if (!acceptPreparation) throw {code:prepareRefusal, message:"The tasks fixture does not start agents."};
      const now = Math.floor(Date.now() / 1000);
      const run = { kind: args.method === "runs.prepare_managed" ? "managed" : "external_terminal", id: input.id, revision: 1, updated_at: 1, task_id: input.task_id, source_revision: input.source_revision, task_title: tasks.find(task => task.id === input.task_id)?.title ?? "", repository_id: input.repository_id, provider: input.provider, permission_mode: input.permission_mode, state: "prepared", cwd: input.repo_path, created_at: now, expires_at: now + 300, session_id: null, exit_code: null, reason: "", outcome_uncertain: false };
      fixtureRuns.set(run.id, run);
      return JSON.stringify({ok: true, item: run});
    }
    case "runs.launch_managed": {
      const run = fixtureRuns.get(input.id);
      if (!run) throw {code:"not_found", message:"Fixture has no such attempt"};
      Object.assign(run, { state: "running", revision: run.revision + 1, session_id: "managed-session", provider_state: "running", provider_thread_id: "managed-thread", provider_turn_id: "managed-turn", effective_configuration: JSON.stringify({sandbox: {type: "readOnly"}, approvalPolicy: "on-request"}), output: "", output_truncated: false });
      return JSON.stringify({ok: true, item: run});
    }
    // `ORDER BY position,id`, as the store lists them. Array order would make
    // a reorder look persisted whether or not its position was written.
    case "workspaces.list": return JSON.stringify(page([...workspaces].sort((a, b) => a.position - b.position || (a.id < b.id ? -1 : 1))));
    case "workspaces.get": return JSON.stringify({ok: true, item: workspaces.find(space => space.id === input.id) ?? workspace});
    case "workspaces.put": {
      workspaceWrites.push(structuredClone(input));
      const index = workspaces.findIndex(space => space.id === input.id);
      if (index < 0 && input.expected_revision === 0) {
        const created = { description: "", icon: "", color: "", pinned: false, archived: false, ...input, revision: 1, updated_at: 1, repository_count: (input.repository_ids ?? []).length };
        delete created.expected_revision; delete created.request_id;
        workspaces.push(created);
        return JSON.stringify({ok: true, item: created});
      }
      if (index < 0) throw {code:"not_found", message:"Fixture has no such workspace"};
      if (workspaces[index].revision !== input.expected_revision) throw {code:"conflict", message:"Fixture workspace changed elsewhere"};
      const saved = { ...workspaces[index], ...input, revision: workspaces[index].revision + 1, repository_count: (input.repository_ids ?? workspaces[index].repository_ids).length };
      workspaces[index] = saved;
      return JSON.stringify({ok: true, item: saved});
    }
    case "enhancements.wake": case "enhancements.worker": return JSON.stringify({ok:true, state:"disabled", reason:"", task_id:"", proposal_id:"", next_check_at:0});
    case "attention.list": {
      const result = JSON.stringify(page([]));
      return result;
    }
    case "runs.list": return JSON.stringify(page([]));
    case "enhancements.configuration": {
      if(failConfiguration) throw {code:"worker_error",message:"Manvi temporarily unavailable"};
      return JSON.stringify({ok:true,provider:"local",model:blankConfiguration ? "" : "quick-fixture",model_source:"fixture",providers:["local"]});
    }
    // As the store lists them: `newest` is newest first, `states` filters, and
    // the page is `limit` long with a cursor past it. The fixture used to return
    // every attempt in one page, so nothing here could ever be on page two.
    case "enhancements.list": {
      let items = [...proposals.values()].filter(p => p.task_id === input.task_id && (!input.states || input.states.includes(p.state)));
      if (input.newest) items = items.reverse();
      return JSON.stringify(page(items, Number(input.cursor ?? 0), input.limit ?? 200));
    }
    case "enhancements.get": enhancementReads.push(input.id); return JSON.stringify({ok:true,item:proposals.get(input.id)});
    case "enhancements.create": case "enhancements.generate": case "enhancements.complete": case "enhancements.accept": case "enhancements.undo": case "enhancements.dismiss": {
      enhancementWrites.push({method:args.method,...structuredClone(input)});
      if(receipts.has(input.request_id)) return receipts.get(input.request_id);
      let proposal = proposals.get(input.id);
      if(args.method === "enhancements.create") {
        const source = structuredClone(tasks.find(task=>task.id === input.task_id));
        proposal = {id:input.id,revision:1,updated_at:1,task_id:input.task_id,source_revision:source.revision,source,fields:input.fields,state:"pending",provider:input.provider,model:input.model,automatic:false,created_at:Math.floor(Date.now()/1000),expires_at:Math.floor(Date.now()/1000)+60};
      } else {
        if(!proposal || proposal.revision !== input.expected_revision) throw {code:"revision_conflict",message:"Suggestion changed"};
        proposal = {...proposal,revision:proposal.revision+1};
        if(args.method === "enhancements.generate") { proposal.state="running"; proposal.worker_id="worker"; }
        if(args.method === "enhancements.complete") {
          // Mirrors the store: a completion carries a proposal or a reason it
          // failed, never both, and never a field nobody asked for.
          if(input.failure !== undefined) { proposal.state="failed"; proposal.failure=input.failure; }
          else {
            for(const field of ["title","description"]) {
              if(input[field] !== undefined && !proposal.fields.includes(field)) throw {code:"invalid_input",message:"a proposal cannot change a field that was not requested"};
              if(input[field] === undefined && proposal.fields.includes(field)) throw {code:"invalid_input",message:`missing proposed ${field}`};
            }
            proposal.state="ready";
            proposal.proposed={...Object.fromEntries(proposal.fields.map(field=>[field,input[field]]))};
            proposal.rationale=input.rationale ?? "";
          }
        }
        if(args.method === "enhancements.dismiss") { proposal.state="cancelled"; proposal.failure="Cancelled by user"; }
        if(args.method === "enhancements.accept" || args.method === "enhancements.undo") {
          const index = tasks.findIndex(task=>task.id===proposal.task_id), current=tasks[index];
          if(current.revision !== input.expected_task_revision) throw {code:"revision_conflict",message:"Task changed"};
          const fields = args.method === "enhancements.accept" ? input.fields : proposal.accepted_fields;
          const source = args.method === "enhancements.accept" ? proposal.proposed : proposal.source;
          tasks[index] = {...current,...Object.fromEntries(fields.map(field=>[field,source[field]])),revision:current.revision+1};
          proposal.state=args.method === "enhancements.accept" ? "accepted" : "undone"; proposal.accepted_fields=fields;
        }
      }
      proposals.set(proposal.id,proposal);
      const result=JSON.stringify({ok:true,item:proposal}); receipts.set(input.request_id,result);
      if(loseEnhancement && args.method === "enhancements.accept") {loseEnhancement=false;throw {code:"transport_error",message:"Lost enhancement reply"};}
      return result;
    }
    case "items.list": {
      if (failList) throw {code: "store_error", message: "Fixture task storage offline"};
      // dc-store schema 11: `deleted` reads deleted tasks instead of live
      // ones, `archived` filters on the flag (omitted: both), `status` is
      // optional, and `order` picks the key the store pages on.
      const matching = tasks.filter(task => deleted.has(task.id) === (input.deleted === true) && (input.status === undefined || task.status === input.status) && (input.archived === undefined || task.archived === input.archived) && (!input.repository_id || task.repository_ids.includes(input.repository_id)) && (!input.workspace_id || task.repository_ids.some(id => workspace.repository_ids.includes(id))) && (!input.query || task.title.toLowerCase().includes(input.query.toLowerCase())))
        // `ORDER BY t.position,t.id`, the same key the store pages on. The
        // fixture used to page in array order, so `items.put` — which appends
        // — moved an edited task to the end of its new column and off the
        // first page. Paging is what this fixture is for, so the order it
        // pages in has to be the real one.
        .sort((a, b) => {
          const byId = a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
          if (input.order === "completed") return (b.completed_at ?? 0) - (a.completed_at ?? 0) || byId;
          if (input.order === "updated") return b.updated_at - a.updated_at || byId;
          return a.position - b.position || byId;
        });
      const result = JSON.stringify(page(matching, Number(input.cursor ?? 0), input.limit));
      if (holdSearch && input.query === "older") return new Promise(resolve => heldSearch.push(() => resolve(result)));
      return result;
    }
    case "items.get": { const found = tasks.find(task => task.id === input.id); const result = JSON.stringify({ok: true, item: found ? { logs: "", ...found } : found}); if (holdGet) return new Promise(resolve => { releaseGet = () => resolve(result); }); return result; }
    case "items.brief.get": {
      const task = tasks.find(item => item.id === input.id);
      if (!task || deleted.has(task.id)) throw {code:"not_found", message:"Fixture has no such task"};
      if (task.revision !== input.expected_revision) throw {code:"revision_conflict", message:"Reload the task before exporting"};
      const repositories = task.repository_ids.map(id => {
        const repo = repos.find(item => item.id === id);
        return { id, revision: repo?.revision ?? 1, updated_at: repo?.updated_at ?? 1, name: repo?.name ?? id };
      });
      const home = task.home_workspace_id ? workspaces.find(space => space.id === task.home_workspace_id) : null;
      const logs = typeof task.logs === "string" && task.logs.trim()
        ? `\n## Raw logs\nPasted evidence. Keep stack frames, timestamps, error codes and quoted text exactly as written.\n\n\`\`\`\n${task.logs}\n\`\`\`\n`
        : "";
      const markdown = `# Task brief v1\n\n## Title\n${task.title}\n\nTask: ${task.id} (revision ${task.revision})\n\n## Description\n${task.description || ""}\n${logs}`;
      return JSON.stringify({ok:true, item:{ id: task.id, revision: task.revision, updated_at: task.updated_at, format_version: 1, task: { logs: "", ...task }, repositories, workspace: home ? { id: home.id, revision: home.revision, updated_at: home.updated_at, name: home.name } : null, markdown }});
    }
    case "items.delete": {
      deleteWrites.push(structuredClone(input));
      if(holdDelete) await new Promise(resolve=>{releaseDelete=resolve;});
      if (receipts.has(input.request_id)) return receipts.get(input.request_id);
      const previous = tasks.find(task => task.id === input.id);
      if (!previous || deleted.has(input.id) || previous.revision !== input.expected_revision) throw {code:"revision_conflict", message:"Task changed before deletion."};
      deleted.add(input.id);
      previous.revision += 1; previous.updated_at = Math.floor(Date.now() / 1000);
      const result = JSON.stringify({ok:true, item:{...previous, deleted:true}});
      receipts.set(input.request_id, result);
      if (corruptDelete) { corruptDelete = false; return JSON.stringify({ok:false}); }
      if (loseDelete) { loseDelete = false; throw {code:"transport_error", message:"Lost delete reply"}; }
      return result;
    }
    // The host's merge (intake_merge.rs). The fixture keeps its reply shape
    // and its two effects the board reads: sources deleted, target rewritten.
    // `mergeFailSource` makes that source's delete fail after its reason is
    // recorded, which the host reports as a partial merge.
    case "items.merge": {
      mergeWrites.push(structuredClone(input));
      const target = tasks.find(task => task.id === input.into.id);
      if (!target || target.revision !== input.into.expected_revision) throw {code:"revision_conflict", message:"This task changed since the board drew it."};
      const rows = input.sources.map(source => {
        const task = tasks.find(item => item.id === source.id);
        if (source.id === mergeFailSource) return { task_id: source.id, item_id: source.id, title: task.title, outcome: "not_merged", copied_revision: task.revision, detail: "Its merge reason is recorded on it, but deleting it failed (fixture), so it is still on the board." };
        deleted.add(source.id); task.revision += 1;
        return { task_id: source.id, item_id: source.id, title: task.title, outcome: "merged", copied_revision: task.revision - 1, detail: null };
      });
      target.revision += 1; target.description += rows.filter(row => row.outcome === "merged").map(row => `\n\n## Merged from ${row.item_id}`).join("");
      const ok = rows.every(row => row.outcome !== "not_merged");
      return JSON.stringify({ ok, outcome: ok ? "merged" : "partial", item_id: target.id, revision: target.revision, sources: rows, next_step: ok ? null : "Run the same merge again to finish it." });
    }
    case "items.restore": {
      restoreWrites.push(structuredClone(input));
      if (receipts.has(input.request_id)) return receipts.get(input.request_id);
      const previous = tasks.find(task => task.id === input.id);
      if (!previous) throw {code:"not_found", message:"Fixture has no such task"};
      if (!deleted.has(input.id)) throw {code:"invalid_state", message:"This task is not deleted."};
      if (previous.revision !== input.expected_revision) throw {code:"conflict", message:"Task changed since it was deleted."};
      deleted.delete(input.id);
      previous.revision += 1; previous.updated_at = Math.floor(Date.now() / 1000);
      const result = JSON.stringify({ok:true, item:{...previous}});
      receipts.set(input.request_id, result);
      return result;
    }
    case "items.put": {
      writes.push(structuredClone(input));
      if (failIssueLinkOnce && input.labels?.some(label => label.startsWith("issue-"))) {
        failIssueLinkOnce = false;
        throw {code:"conflict", message:"Task changed. Reload the saved task."};
      }
      if (holdSave) await new Promise(resolve => { releaseSave = resolve; });
      if (receipts.has(input.request_id)) return receipts.get(input.request_id);
      const previous = tasks.find(task => task.id === input.id);
      if ((previous?.revision ?? 0) !== input.expected_revision) throw {code:"conflict", message:"Task changed. Reload the saved task."};
      const { expected_revision, request_id, ...draft } = input;
      if ("completed_at" in draft) throw {code:"invalid_input", message:"unknown field completed_at"};
      // A field the request omits keeps its stored value, as dc-store's `put_item` does.
      const kept = { archived: previous?.archived ?? false, checklist: previous?.checklist ?? [], links: previous?.links ?? [] };
      const item = {...kept, ...draft, due_at: draft.due_at ?? null, completed_at: completedAt(previous, draft.status), revision: expected_revision + 1, updated_at: 2};
      tasks = [...tasks.filter(task => task.id !== item.id), item];
      const result = JSON.stringify({ok:true, item}); receipts.set(request_id, result);
      if (corruptSave) { corruptSave = false; return JSON.stringify({ok:true, item:{}}); }
      if (loseSave) { loseSave = false; throw {code:"transport_error", message:"Fixture lost the save reply"}; }
      return result;
    }
    default: unknown.push(args.method); throw Error(`Unconfigured method: ${args.method}`);
  }
});
applyPlatformClass();
themeStore.setTheme(params.get("theme") === "light" ? "light" : "dark");
void harnessStore.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "quick-fixture" });
window.addEventListener("error", e => crashes.push(e.message));
window.addEventListener("unhandledrejection", e => crashes.push(String(e.reason)));
const root = document.getElementById("app");
// The board's polls (a running Manvi suggestion, for one) pause while the
// window is in the background. A person is looking at the board for the whole
// run, including after a real window blur (see harness/foreground.ts). The one
// background check below sets `document.hidden`, which outranks it.
holdForeground();
mount(TasksHost, {target: root});
let confirmations = 0, confirmAnswer = true;
const settle = async (ms = 30) => {
  await tick(); await new Promise(resolve => setTimeout(resolve, ms)); await tick();
  const pendingPrompt = get(promptState);
  const prompt = pendingPrompt ? [...document.querySelectorAll('[role="dialog"]')].filter(node => node.getAttribute("aria-label") === pendingPrompt.options.title).at(-1) : null;
  if (prompt && pendingPrompt.options.title.startsWith("Delete")) { button(pendingPrompt.options.confirmLabel, prompt)?.click(); await new Promise(resolve => setTimeout(resolve, 0)); await tick(); }
  if (prompt && pendingPrompt.options.title.startsWith("Discard")) { confirmations++; [...prompt.querySelectorAll("button")].find(button => button.textContent.trim() === (confirmAnswer ? "Discard edits" : "Keep editing"))?.click(); await new Promise(resolve => setTimeout(resolve,0)); await tick(); }
  if (prompt && pendingPrompt.options.title === "Reload saved task?") { button("Reload", prompt)?.click(); await new Promise(resolve => setTimeout(resolve,0)); await tick(); }
};
const wait = async predicate => { const deadline = Date.now() + 15_000; while (Date.now() < deadline) { if (predicate()) return; await settle(); } throw Error(`Timed out waiting for task UI [${describeForeground()}]`); };
const aliases = {"Quick Enhance":"Quick Enhance…","Add task to Ready":"New task in Ready","Close workspace details":"Close workspace settings", "Refresh tasks":"Refresh", "List view":"List", "Board view":"Board", "Duplicate task…":"Duplicate…", "Delete task":"Delete", "Retry deletion":"Retry delete", "Copy for agent":"Copy task for an AI agent"};
const button = (text, within = root) => [...within.querySelectorAll("button")].find(el => {
  const names = [text, aliases[text]].filter(Boolean);
  return names.includes(el.textContent.trim()) || names.includes(el.getAttribute("aria-label")) || names.includes(el.querySelector(":scope > span.flex-1")?.textContent.trim());
});
const field = (text) => [...root.querySelectorAll(".task-editor label")].find(el => el.firstChild?.textContent.trim() === (text === "Due date" || text === "Due" ? "Due" : text === "Custom type" ? "Type" : text))?.querySelector("input,textarea,select");
// The label carries the engine's name, so matching on text alone stopped
// finding this button the moment a second engine existed. The class is the
// stable handle; the text lookups stay first so failures still name a label.
const enhanceButton = () => button("Improve with Manvi") ?? button("Enhance with Manvi") ?? button("Draft with Manvi")
  ?? root.querySelector(".manvi-assist button.ask");
// Scoped to board cards. The archive dock's rows carry `data-card-id` too, so
// an unscoped lookup returned an archive row whenever the dock was open — and
// an archive row has no context menu, no column and no drag, so every check
// built on it would have failed for a reason that had nothing to do with what
// it was testing.
const card = id => root.querySelector(`[data-task-card][data-card-id="${id}"]`);
// The repository control is a trigger on the pane plus a popover portaled to
// the body, which is outside `root` — so these reach it through `document`,
// the same way the context-menu helpers above already do.
const repoToggle = () => editor()?.querySelector("[data-task-repo-picker] .repo-trigger");
// Due is not a labelled input any more, so `field("Due")` cannot reach it.
const dueTrigger = () => editor()?.querySelector('[data-testid="task-due-trigger"]');
const editorSection = (id) => editor()?.querySelector(`[data-editor-section="${id}"]`);
const assistBody = () => editor()?.querySelector('[data-testid="task-assist-body"]');
const assistToggle = () => editor()?.querySelector('[data-testid="task-assist-toggle"]');
const onScreen = el => Boolean(el) && el.getClientRects().length > 0;
const repoPopup = () => document.querySelector("[data-task-repo-popup]");
const addRepoMenu = () => document.querySelector("[data-add-repo-popup]");
const addRepoTrigger = () => root.querySelector("[data-add-repo] button");
const workspaceTab = name => [...root.querySelectorAll('nav[aria-label="Task scopes"] button')].find(el => el.textContent.trim().startsWith(name));
const fitsViewport = (el, pad = 1) => {
  if (!el) return false;
  const r = el.getBoundingClientRect();
  return r.width > 0 && r.height > 0 && r.left >= -pad && r.top >= -pad && r.right <= innerWidth + pad && r.bottom <= innerHeight + pad;
};
const uncropped = (el) => Boolean(el) && el.scrollWidth <= el.clientWidth + 1;
const openRepoPicker = async () => { if (!repoPopup()) repoToggle()?.click(); await settle(80); return repoPopup(); };
// Dismissed the way a reader dismisses it: a pointerdown outside. The picker
// listens on the capture phase, so this reaches it from `body`.
const closeRepoPicker = async () => { if (repoPopup()) document.body.dispatchEvent(new PointerEvent("pointerdown", {bubbles:true})); await settle(60); };
const repoRow = name => [...(repoPopup()?.querySelectorAll(".repo-row") ?? [])].find(row => row.querySelector(".repo-name")?.textContent.trim() === name);
// The whole closed control, not just the button: the trigger names the linked
// repositories as chips and the `summaryLine` sentence sits directly under it,
// because chips cannot say "no primary chosen". Both are what a reader sees
// without opening anything, so both count as the summary.
const repoSummaryText = () => editor()?.querySelector("[data-task-repo-picker]")?.textContent ?? "";
const editor = () => root.querySelector(".task-editor");
const check = (name, pass) => results.push({name, pass:Boolean(pass)});
const change = async (el, value, event = "input") => { if(!el) throw Error("Missing form field"); el.value = value; el.dispatchEvent(new Event(event, {bubbles:true})); await settle(); };
const click = async text => {
  if (text === "Clear task search") return change(root.querySelector('[aria-label="Search tasks"]'), "");
  const scope = text === "Delete selected tasks" || text === "Clear task selection" ? root.querySelector('.selection') : root;
  const el = button(text === "Delete selected tasks" ? "Delete" : text === "Clear task selection" ? "Clear" : text, scope); if (!el) throw Error(`Missing button: ${text}`); el.click(); await settle(); };
const moveThroughMenu = async (id, status) => {
  card(id).dispatchEvent(new MouseEvent("contextmenu",{bubbles:true,cancelable:true,clientX:50,clientY:80})); await settle();
  const menu = () => document.querySelector('[role="menu"][aria-label="Task actions"]');
  button("Move to…",menu()).click(); await settle(); button(status,menu()).click(); await settle(150);
};
await wait(() => card("task-1"));
if (params.has("check")) {
  try {
    // These assertions exercise the pre-fix defects through production components.
    await click("GitPulse fixture"); await settle(400);
    check("repository prop changes reload the board scope", !card("task-3") && Boolean(card("task-1")));
    await click("Manvi fixture"); await settle(400);
    check("switching repository tabs replaces prior task cards", Boolean(card("task-3")) && !card("task-1"));
    await click("Global fixture"); await settle(400);
    card("task-1").click(); await wait(editor);
    const title = field("Title"); await change(title, "Unsaved task title");
    confirmations = 0;
    confirmAnswer = false;
    await click("New workspace");
    check("opening a workspace cannot silently discard a task draft", Boolean(editor()) && field("Title")?.value === "Unsaved task title" && confirmations === 1);
    confirmAnswer = true;
    if(editor()) await click("Close task details");
    // Continue even when the pre-fix workspace action incorrectly replaced the sheet.
    const workspaceClose = root.querySelector('[aria-label="Close workspace details"]'); workspaceClose?.click(); await settle();
    card("task-1").click(); await wait(editor);
    holdSave = true; await change(field("Title"), "Saved task title");
    await click("Save task");
    check("task close is disabled while its save is in flight", button("Close task details").disabled);
    holdSave = false; releaseSave(); await wait(() => !button("Save task")?.disabled);
    await click("Close task details");
    const ready = root.querySelector('[data-task-column="ready"]');
    (button("Load more Ready tasks", ready) ?? button("Next", ready)).click(); await settle(150);
    check("loading more tasks keeps already loaded cards", ready.querySelectorAll("[data-task-card]").length === 32 && Boolean(card("task-10")));
    await click("New workspace");
    await change(root.querySelector('.workspace-editor input'), "Unsaved workspace");
    confirmations = 0; confirmAnswer = false;
    await click("New task");
    check("opening a task preserves unsaved workspace edits", Boolean(root.querySelector('.workspace-editor')) && confirmations === 1);
    confirmAnswer = true;
    if(editor()) await click("Close task details");
    button("Close workspace settings")?.click(); await settle();
    confirmations = 0; confirmAnswer = false;
    card("task-2").focus(); card("task-2").click(); await wait(editor);
    check("opening the sheet focuses its title", document.activeElement === field("Title"));
    check("type offers all supported task kinds and permits custom names", field("Type") instanceof HTMLSelectElement && field("Type").options.length === 8 && [...field("Type").options].some(option => option.value === "__custom__"));
    // Repositories lead the sheet. A task cannot be saved without one, and
    // this used to be the last control on the pane, below the criteria box.
    // Collapsing the row list into a dropdown is only an improvement if the
    // closed control still answers what the list answered. Its label is
    // `summaryLine`, the same sentence the inline summary carried.
    check("the repository control is on the pane and already says what is linked",
      Boolean(repoToggle()) && repoToggle().getClientRects().length > 0 && !repoToggle().closest("details")
      && repoToggle().getAttribute("aria-expanded") === "false"
      && repoSummaryText().includes("primary GitPulse"));
    check("the repository control comes before the title on the pane",
      Boolean(repoToggle()) && (repoToggle().compareDocumentPosition(field("Title")) & Node.DOCUMENT_POSITION_FOLLOWING));
    // A saved task used to open on the compose surface labelled "Quick add":
    // notes, engine picker, and an accepted suggestion filled the viewport
    // while title sat below the fold. Title first, model last and folded.
    check("a saved task leads with the title, not a compose surface",
      onScreen(field("Title")) && fitsViewport(field("Title"))
      && Boolean(editorSection("title")) && Boolean(editorSection("assist"))
      && (editorSection("title").compareDocumentPosition(editorSection("assist")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0
      && Boolean(assistBody()?.hidden)
      && !onScreen(editor().querySelector(".notes-label textarea"))
      && !(editor().querySelector("h3") && [...editor().querySelectorAll("h3")].some(h => /quick add/i.test(h.textContent))));
    check("a saved task folds the model until the reader asks",
      Boolean(assistToggle())
      && assistToggle().getAttribute("aria-expanded") === "false"
      && assistToggle().textContent.trim() === "Show");
    const titleTop = field("Title").getBoundingClientRect().top;
    assistToggle().click(); await settle();
    check("showing the assist on a saved task does not move the title below it",
      onScreen(field("Title"))
      && Math.abs(field("Title").getBoundingClientRect().top - titleTop) <= 1
      && !assistBody()?.hidden
      && assistToggle().getAttribute("aria-expanded") === "true"
      && onScreen(editor().querySelector(".notes-label textarea"))
      && (editorSection("title").compareDocumentPosition(editorSection("assist")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0);
    assistToggle().click(); await settle();
    check("hiding the assist again leaves the title on screen",
      Boolean(assistBody()?.hidden) && onScreen(field("Title")) && assistToggle().getAttribute("aria-expanded") === "false");
    await openRepoPicker();
    const primaryRadio = () => [...(repoPopup()?.querySelectorAll('input[type="radio"]') ?? [])].find(el => el.checked);
    check("linking and the primary choice are one control, not two that can disagree",
      Boolean(primaryRadio())
      && Boolean(primaryRadio().closest(".repo-row")?.querySelector('input[type="checkbox"]')?.checked)
      && repoSummaryText().includes("primary GitPulse")
      && !editor().textContent.includes("Linked repositories"));
    // A popover inside `.sheet-body` would be clipped by the scroller it is
    // trying to escape, so it is portaled — and then clamped into the viewport.
    const popupRect = () => repoPopup()?.getBoundingClientRect();
    check("the repository dropdown escapes the sheet's own clip, and fits the viewport",
      Boolean(repoPopup()) && !editor().contains(repoPopup())
      && popupRect().width > 0 && popupRect().right <= innerWidth + 1 && popupRect().bottom <= innerHeight + 1 && popupRect().left >= -1);
    // The sheet also closes on Escape. One Escape must dismiss one thing.
    repoPopup()?.querySelector("input")?.focus();
    document.activeElement?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle(80);
    check("Escape in the repository dropdown closes the dropdown, not the task",
      !repoPopup() && Boolean(editor()) && document.activeElement === repoToggle());
    // The anchor scrolling away must not leave the popover behind it. Guarded
    // throughout: if the Escape above closed the whole sheet instead of the
    // dropdown, `editor()` is null here, and an unguarded probe would throw
    // and hide every check after it behind one stack trace.
    if (!editor()) { card("task-2").click(); await wait(editor); }
    await openRepoPicker();
    editor()?.querySelector(".sheet-body")?.dispatchEvent(new Event("scroll"));
    await settle(80);
    check("scrolling the sheet dismisses the repository dropdown instead of leaving it floating",
      Boolean(editor()) && !repoPopup() && repoToggle()?.getAttribute("aria-expanded") === "false");
    const paneTab = id => editor()?.querySelector(`[data-sheet-tab="${id}"]`);
    check("a saved task opens on Task and draws exactly one pane",
      [...editor().querySelectorAll("[data-sheet-tab]")].map(tab => tab.getAttribute("data-sheet-tab")).join(",") === "task,agent"
      && paneTab("task").getAttribute("aria-selected") === "true"
      && [...editor().querySelectorAll(".pane")].filter(onScreen).length === 1);
    // This check used to assert the opposite for the assist: it lived behind
    // its own tab while the suggestions it produced were drawn on this one, so
    // the reader pressed a button on one pane and read the result on another.
    // Both halves are asserted, because "on screen" alone would pass for a
    // layout that had simply stopped hiding everything.
    check("the assist writes where the reader is, and only the agent panel is a separate pane",
      Boolean(editor().querySelector('[aria-label="Manvi task assist"]'))
      && Boolean(editorSection("assist"))
      && !onScreen(editor().querySelector('[aria-label="Task agent runs"]')));
    // Schedule and labels are on this pane too, beside the description rather
    // than behind a second tab.
    check("the merged pane carries schedule and labels beside the task text",
      onScreen(field("Title")) && onScreen(field("Owner")) && onScreen(dueTrigger()) && onScreen(field("Labels")));
    paneTab("task").focus();
    paneTab("task").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await settle();
    check("arrow keys move the sheet's tab strip and the focus with it",
      paneTab("agent").getAttribute("aria-selected") === "true" && document.activeElement === paneTab("agent")
      && onScreen(editor().querySelector('[data-testid="task-handoff-form"]')));
    check("the agent pane offers the same handoff form the board sheet uses", onScreen(editor().querySelector('[data-testid="task-handoff-form"]')));
    paneTab("task").click(); await settle();
    // History was a collapsed `<details>` whose rows carried no time, above a
    // review that was suppressed whenever a ready suggestion was showing
    // beside the fields — so changing the selection could do nothing visible.
    const historyPicker = () => editor().querySelector('[data-testid="task-assist-history"]');
    check("the suggestion picker is on screen the moment the assist is, and is a real picker",
      Boolean(editor().querySelector(".manvi-assist")) && Boolean(historyPicker())
      && historyPicker() instanceof HTMLSelectElement && !editor().querySelector(".history-drawer"));
    check("merged Manvi section has no model input", Boolean(editor().querySelector(".manvi-assist .change-link")) && ![...editor().querySelectorAll(".manvi-assist label")].some(label => label.firstChild?.textContent.trim() === "Model"));
    editor().querySelector(".sheet-body").scrollTop = 900; await settle();
    const saveRect = button("Save task").getBoundingClientRect();
    check("save remains visible while the sheet scrolls", saveRect.top >= 0 && saveRect.bottom < innerHeight);
    await click("Close task details");
    check("a clean task closes without a discard prompt", !editor() && confirmations === 0);
    check("closing restores focus to the task card", document.activeElement === card("task-2"));

    card("task-1").click(); await wait(editor);
    card("task-2").click(); await wait(() => root.querySelectorAll('[aria-label="Open tasks"] [role="tab"]').length === 2);
    check("opening a second task keeps both in the tab strip", root.querySelectorAll('[aria-label="Open tasks"] [role="tab"]').length === 2 && Boolean(card("task-1")?.hasAttribute("data-open-task")) && Boolean(card("task-2")?.hasAttribute("data-open-task")));
    const activeTab = [...root.querySelectorAll('[aria-label="Open tasks"] [role="tab"]')].find(tab => tab.getAttribute("aria-selected") === "true");
    activeTab?.parentElement?.querySelector('[data-testid="task-tab-close"]')?.click();
    await wait(() => field("Title")?.value === "Saved task title");
    check("closing the active tab activates its neighbor", Boolean(editor()) && field("Title")?.value === "Saved task title");
    root.querySelector('[aria-label="Open tasks"] [data-testid="task-tab-close"]')?.click();
    await wait(() => !editor());
    check("closing the last open task removes the editor", !editor() && !root.querySelector('[aria-label="Open tasks"]'));

    // Add-repository lives on the left-rail plus, not the task sheet. The
    // pre-fix menu was a 260px absolute panel inside the 188px navigator
    // scroller: opening it grew the sidebar and cropped the rows. These
    // checks would have failed that geometry. They mutate the catalog, so
    // they restore it before any later picker assertion sees the extras.
    {
      const nav = () => root.querySelector(".navigator");
      const longName = `very-long-repo-name-${"x".repeat(180)}`;
      const longPath = `/fixture/${"deep/".repeat(40)}hostile-repo`;
      const extras = [
        { id: "repo-stress-long", name: longName, revision: 1, updated_at: 1, identity_key: `local:${longPath}/.git`, remote_url: null },
        ...Array.from({ length: 47 }, (_, i) => ({
          id: `repo-stress-${i}`,
          name: `Stress ${String(i).padStart(2, "0")}`,
          revision: 1,
          updated_at: 1,
          identity_key: `local:/fixture/stress-${i}/.git`,
          remote_url: null,
        })),
      ];
      const dropExtras = () => {
        for (let i = repos.length - 1; i >= 0; i--) if (String(repos[i].id).startsWith("repo-stress")) repos.splice(i, 1);
      };
      try {
        await click("Global fixture");
        await wait(() => Boolean(workspaceTab("Fresh space")) && Boolean(addRepoTrigger()));
        const fresh = workspaceTab("Fresh space");
        fresh.click();
        await wait(() => fresh.getAttribute("aria-pressed") === "true" && (addRepoTrigger()?.getAttribute("aria-label") ?? "").includes("Fresh space"));
        await wait(() => !root.querySelector('[aria-label="Refresh"]')?.disabled);
        repos.push(extras[0]);
        await click("Refresh");
        await wait(() => nav()?.textContent.includes("very-long-repo-name-"));
        const beforeScroll = nav()?.scrollWidth ?? 0;
        const beforeClient = nav()?.clientWidth ?? 0;
        for (let i = 0; i < 25 && !addRepoMenu(); i++) { addRepoTrigger()?.click(); await settle(80); }
        const menu = addRepoMenu();
        const longRow = [...(menu?.querySelectorAll(".add-name") ?? [])].find(el => el.textContent.includes("very-long-repo-name-"));
        const line = longRow ? Number.parseFloat(getComputedStyle(longRow).lineHeight) || 16 : 16;
        check("the add-repository menu escapes the navigator instead of expanding it",
          Boolean(menu) && Boolean(nav()) && !nav().contains(menu)
          && nav().scrollWidth <= beforeClient + 1
          && nav().scrollWidth <= beforeScroll + 1);
        check("the add-repository menu stays inside the window", fitsViewport(menu));
        check("a long repository name wraps in the add-repository menu instead of cropping",
          Boolean(longRow) && uncropped(longRow) && longRow.clientHeight > line * 1.5);
        const choose = menu ? button("Choose folder…", menu) : null;
        check("Choose folder remains fully readable in the add-repository menu",
          Boolean(choose) && uncropped(choose));
        menu?.querySelector('[role="menuitem"]')?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        await settle(80);
        check("Escape in the add-repository menu restores focus to the plus",
          !addRepoMenu() && document.activeElement === addRepoTrigger());
        repos.push(...extras.slice(1));
        await wait(() => !root.querySelector('[aria-label="Refresh"]')?.disabled);
        await click("Refresh");
        await wait(() => (nav()?.textContent.match(/Stress \d+/g) ?? []).length >= 20);
        for (let i = 0; i < 25 && !addRepoMenu(); i++) { addRepoTrigger()?.click(); await settle(80); }
        const tall = addRepoMenu();
        check("a long registered list still fits the window and scrolls inside the menu",
          Boolean(tall) && fitsViewport(tall) && tall.scrollHeight > tall.clientHeight && getComputedStyle(tall).overflowY === "auto");
        document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
        await settle(80);
        check("a pointer outside dismisses the add-repository menu", !addRepoMenu());
      } finally {
        if (addRepoMenu()) { document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })); await settle(60); }
        dropExtras();
        workspaceTab("All")?.click();
        await click("Refresh");
        await wait(() => Boolean(card("task-1")));
      }
    }

    await click("Add task to Ready");
    check("column creation uses that column's status", field("Status").value === "ready");
    check("new tasks default to normal priority", field("Priority").value === "2");
    await change(field("Title"), "   ");
    check("whitespace titles cannot be saved", button("Save task").disabled);
    await change(field("Title"), "A concise task");
    await change(field("Acceptance criteria"), "  Result verified  \n\n  Recovery checked  ");
    await change(field("Status"), "review", "change");
    check("select changes are marked as unsaved", editor().querySelector("header small").textContent === "Unsaved");
    loseSave = true; await click("Save task"); await wait(() => button("Retry save"));
    const pendingWrite = writes.at(-1);
    check("uncertain saves lock the draft and expose an exact retry", field("Title").matches(":disabled") && !button("Retry save").disabled);
    await click("Close task details");
    check("uncertain saves cannot be discarded before reconciliation", Boolean(editor()) && button("Close task details").disabled);
    await click("Retry save"); await wait(() => button("Save task") && !field("Title").matches(":disabled"));
    check("retrying a lost save reuses the same mutation identity", writes.at(-1).request_id === pendingWrite.request_id && tasks.find(task => task.id === pendingWrite.id)?.revision === 1);
    check("acceptance criteria are normalized and retained after saving", field("Acceptance criteria").value === "Result verified\nRecovery checked");
    check("creation persists the selected task status", tasks.find(task => task.id === pendingWrite.id)?.status === "review");
    const logsField = field("Raw logs");
    check("the task sheet offers a raw-logs paste surface", Boolean(logsField) && Boolean(editor().querySelector('[data-testid="task-logs"]')));
    const ansiDump = "\u001b[31merror: E42\u001b[0m\r\n    at src/main.rs:12";
    const dt = new DataTransfer(); dt.setData("text/plain", ansiDump);
    logsField.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: dt }));
    await settle();
    if (logsField.value.includes("\u001b") || !logsField.value.includes("E42")) await change(logsField, ansiDump);
    check("pasted ANSI logs keep the error code and drop the escape codes", logsField.value.includes("error: E42") && logsField.value.includes("src/main.rs:12") && !logsField.value.includes("\u001b"));
    const copiesBefore = copies.length;
    await click("Copy logs"); await settle();
    check("Copy logs puts the sanitized dump on the clipboard", copies.at(-1)?.includes("error: E42") && !copies.at(-1)?.includes("\u001b") && copies.length > copiesBefore);
    await click("Save task"); await wait(() => tasks.find(task => task.id === pendingWrite.id)?.logs?.includes("E42"));
    check("saving the sheet persists the raw logs", tasks.find(task => task.id === pendingWrite.id)?.logs?.includes("error: E42") && !tasks.find(task => task.id === pendingWrite.id)?.logs?.includes("\u001b"));
    await click("Copy for agent"); await settle();
    check("Copy for agent includes the saved raw logs as evidence", copies.at(-1)?.includes("## Raw logs") && copies.at(-1)?.includes("error: E42"));
    await click("Close task details");
    card(pendingWrite.id).click(); await wait(editor);
    check("reopening the sheet restores the saved raw logs", field("Raw logs")?.value.includes("error: E42") && field("Raw logs")?.value.includes("src/main.rs:12"));
    const huge = `${"x".repeat(256 * 1024 + 50)}the reproduction ends with E999`;
    await change(field("Raw logs"), huge);
    check("an oversized paste is cut with a visible notice instead of silently dropping the tail", field("Raw logs").value.includes("[logs truncated:") && editor().textContent.includes("bytes kept") && !field("Raw logs").value.includes("E999"));
    confirmAnswer = true; await click("Close task details"); await settle();

    const taskOne = tasks.find(task => task.id === "task-1"); taskOne.kind = "custom-category";
    card("task-1").click(); await wait(editor);
    check("existing custom task types remain editable without data loss", field("Type") instanceof HTMLInputElement && field("Type").value === "custom-category");
    await change(field("Type"), "");
    check("custom type creation requires a name", field("Type")?.required && button("Save task").disabled);
    await change(field("Type"), "experiment");
    await change(field("Description"), "A shorter brief.");
    holdSave = true; await click("Save task");
    const inFlightWrites = writes.length;
    editor().querySelector("form").dispatchEvent(new Event("submit", {bubbles:true, cancelable:true})); await settle();
    check("duplicate submissions cannot create concurrent saves", writes.length === inFlightWrites);
    holdSave = false; releaseSave(); await wait(() => !field("Title").matches(":disabled"));
    check("description changes and custom task types round-trip", tasks.find(task => task.id === "task-1")?.description === "A shorter brief." && tasks.find(task => task.id === "task-1")?.kind === "experiment");
    corruptSave = true; await change(field("Description"), "Updated brief after protocol failure.");
    await click("Save task"); await settle(120);
    const malformedWrite = writes.at(-1);
    check("malformed save replies retain the pending write", Boolean(button("Retry save")) && field("Title").matches(":disabled"));
    if (button("Retry save")) {
      await click("Retry save"); await wait(() => button("Save task") && !field("Title").matches(":disabled"));
      check("malformed save recovery reuses the committed receipt", writes.at(-1).request_id === malformedWrite.request_id && tasks.find(task => task.id === "task-1").revision === malformedWrite.expected_revision + 1);
    }
    confirmAnswer = true; await click("Close task details");

    let beforeMove = tasks.find(task => task.id === "task-2");
    await moveThroughMenu("task-2", "Done");
    await wait(() => card("task-2")?.closest('[data-task-column="done"]'));
    const afterMove = tasks.find(task => task.id === "task-2");
    check("card status choices persist one revision without opening a sheet", afterMove.revision === beforeMove.revision + 1 && !editor());
    check("status changes preserve both repository links and task text", afterMove.repository_ids.length === 2 && afterMove.description === beforeMove.description);
    // A stale card must not overwrite a concurrent edit.
    const stale = tasks.find(task => task.id === "task-3"); stale.revision++;
    await moveThroughMenu("task-3", "Done");
    await wait(() => root.textContent.includes("This task changed while you were moving it"));
    check("stale status choices report a conflict without changing the task", stale.status === "backlog" && card("task-3").closest('[data-task-column="backlog"]'));
    await click("Refresh tasks"); await settle(300);

    const search = root.querySelector('[aria-label="Search tasks"]');
    await change(search, "does not exist"); await settle(400);
    check("empty searches explain the result and hide previous tasks", root.textContent.includes("No tasks match") && root.querySelectorAll('[data-task-card]').length === 0);
    await click("Clear task search"); await wait(() => card("task-1"));
    check("clearing search restores the board", Boolean(card("task-1")));
    holdSearch = true; await change(search, "older"); await wait(() => heldSearch.length === 6);
    await change(search, "Ready task 01"); await wait(() => card("task-10"));
    heldSearch.forEach(release => release()); holdSearch = false; heldSearch = []; await settle();
    check("late search responses cannot replace newer results", Boolean(card("task-10")) && root.querySelectorAll('[data-task-card]').length === 1);
    failList = true; await change(search, "offline"); await settle(400);
    check("failed loads remain distinct from an empty task list", root.textContent.includes("Fixture task storage offline") && root.textContent.includes("Tasks unavailable") && !root.textContent.includes("No tasks match"));
    failList = false; await click("Retry loading tasks"); await settle(300);
    check("a failed search can be retried", root.textContent.includes("No tasks match") && !root.textContent.includes("Fixture task storage offline"));
    await click("Clear task search"); await wait(() => card("task-1"));

    await click("Developer tools"); await wait(() => Boolean(card("task-3")) && !card("task-1"));
    await click("New task");
    // The picker owns both halves now, so "which repository is primary" is
    // read off the checked radio rather than a separate select.
    // Read off the trigger while closed, and off the rows while open: the two
    // must agree, which is the whole reason the trigger carries the summary.
    const primaryName = () => repoPopup()?.querySelector('.repo-row input[type="radio"]:checked')?.closest(".repo-row")?.querySelector(".repo-name")?.textContent.trim();
    await openRepoPicker();
    check("workspace tasks default to a repository in that workspace",
      primaryName() === "Manvi" && repoSummaryText().includes("primary Manvi"));
    check("the picker marks which repositories the home workspace already holds",
      repoPopup()?.querySelector(".repo-row.linked .repo-mark")?.textContent.trim() === "In workspace");
    await closeRepoPicker();
    await change(field("Title"), "Workspace draft");
    await click("GitPulse fixture"); await settle(400);
    await openRepoPicker();
    check("repository tab switches preserve the open draft's repository", field("Title").value === "Workspace draft" && primaryName() === "Manvi");
    await closeRepoPicker();
    confirmAnswer = true; await click("Close task details");

    // A workspace with no member repositories. Every one of these used to
    // fail: New task was disabled with nothing on screen saying why, the
    // empty state offered a New task anyway that opened an unsaveable sheet,
    // and nothing in the board could give the workspace a repository.
    // The navigator only exists in the global fixture, so restore it first.
    // Matched on a prefix, not the whole label, so the rest of this block
    // still runs against a build whose navigator says nothing about members.
    await click("Global fixture"); await settle(400);
    check("the navigator says which workspaces are empty before one is opened",
      workspaceTab("Fresh space").textContent.trim() === "Fresh space · Empty");
    workspaceTab("Fresh space").click(); await settle(400);
    check("an empty workspace still offers New task, and says what the sheet will ask for",
      button("New task").disabled === false
      && root.querySelector('[data-testid="task-create-hint"]')?.textContent.includes("Fresh space has no repositories yet"));
    check("the empty board's own New task agrees with the header instead of contradicting it",
      Boolean(button("New task", root.querySelector(".board-main"))) && root.textContent.includes("Fresh space has no repositories yet"));
    // Every step below is guarded rather than allowed to throw. A build that
    // withdraws the control records a failed check and lets the rest of the
    // block report; an exception here would hide them behind one stack trace.
    const quickAddField = () => root.querySelector(".quick-add-row input");
    if (quickAddField()) {
      await change(quickAddField(), "A line with no repository");
      quickAddField().dispatchEvent(new KeyboardEvent("keydown", {key:"Enter", bubbles:true, cancelable:true})); await settle(200);
      check("quick add refuses here by naming the marker and the editor, not just failing",
        root.textContent.includes("^name") && root.textContent.includes("Shift+Return"));
      await change(quickAddField(), "");
    } else {
      check("quick add refuses here by naming the marker and the editor, not just failing", false);
    }
    button("New task")?.click(); await settle(400);
    const summaryText = () => repoSummaryText();
    check("New task in an empty workspace opens a sheet at all", Boolean(editor()));
    if (editor()) await change(field("Title"), "First task in a fresh workspace");
    check("the sheet opens with nothing linked and refuses to save until one is",
      summaryText().includes("No repository linked") && button("Save task")?.disabled === true);
    await openRepoPicker();
    repoRow("GitPulse")?.querySelector('input[type="checkbox"]')?.click(); await settle(150);
    check("linking a repository from the picker is what makes the task saveable",
      button("Save task")?.disabled === false && summaryText().includes("primary GitPulse"));
    // Linking must update the closed control too, and must not dismiss it —
    // a picker that shut on every tick could not link two repositories.
    check("linking updates the trigger without closing the dropdown", Boolean(repoPopup()));
    await closeRepoPicker();
    check("the sheet says the link is outside the home workspace and offers the join",
      Boolean(editor()?.textContent.includes("not in Fresh space")) && Boolean(editor() && button("Add to workspace", editor())));
    if (editor() && button("Add to workspace", editor())) { await click("Add to workspace"); await settle(250); }
    await openRepoPicker();
    check("Add to workspace writes the membership and the row stops being an outsider",
      workspaceWrites.some(write => write.id === "workspace-empty" && write.repository_ids.includes("repo-0"))
      && !editor()?.textContent.includes("not in Fresh space")
      && repoRow("GitPulse")?.querySelector(".repo-mark")?.textContent.trim() === "In workspace");
    await closeRepoPicker();
    if (button("Save task")) { await click("Save task"); await settle(300); }
    check("a task created from an empty workspace saves with that workspace as its home",
      tasks.some(task => task.title === "First task in a fresh workspace" && task.home_workspace_id === "workspace-empty" && task.primary_repository_id === "repo-0"));
    confirmAnswer = true; if (editor()) await click("Close task details");
    await click("All"); await wait(() => card("task-1"));

    await click("Global fixture"); await settle(400);
    holdGet = true; card("task-1").click(); await wait(() => releaseGet);
    // Repeated clicks while loading cannot create competing editor requests.
    card("task-2").click(); holdGet = false; releaseGet(); await wait(editor);
    check("opening a task suppresses competing detail loads", field("Title").value === "Saved task title");
    await change(field("Title"), "Keyboard draft");
    confirmAnswer = false;
    field("Title").dispatchEvent(new KeyboardEvent("keydown", {key:"Escape", bubbles:true, cancelable:true})); await settle();
    check("Escape respects the unsaved draft confirmation", field("Title").value === "Keyboard draft");
    confirmAnswer = true;
    field("Title").dispatchEvent(new KeyboardEvent("keydown", {key:"Escape", bubbles:true, cancelable:true})); await settle();
    check("Escape closes the sheet after confirmed discard", !editor());
    confirmAnswer = true;
    card("task-10").dispatchEvent(new MouseEvent("contextmenu", {bubbles:true,cancelable:true,clientX:50,clientY:80})); await settle();
    check("task context menu exposes Quick Enhance and task-specific actions", Boolean(document.querySelector('[role="menu"][aria-label="Task actions"]')) && Boolean(button("Quick Enhance", document)));
    document.querySelector('[role="menu"][aria-label="Task actions"]')?.dispatchEvent(new KeyboardEvent("keydown", {key:"Escape",bubbles:true,cancelable:true})); await settle();
    check("task organization offers board and list layouts", Boolean(button("List view")));
    check("task selection supports explicit bulk actions", card("task-10").getAttribute("aria-keyshortcuts").includes("ContextMenu"));
    card("task-10").click(); await wait(editor);
    corruptDelete = true;
    await click("Delete task");
    const confirmDelete = document.querySelector('[role="dialog"][aria-label="Delete tasks"]');
    if(confirmDelete) { button("Delete 1 task",confirmDelete).click(); await settle(); }
    check("a malformed deletion receipt is never reported as success", Boolean(editor()) || Boolean(document.querySelector('[role="dialog"][aria-label="Delete tasks"]')));
    check("uncertain deletion has an explicit reconciliation retry", Boolean(button("Retry deletion",document)));
    check("task surfaces participate in the shared liquid material", Boolean(root.querySelector('.workbench.bg-background')) && Boolean(root.querySelector('.task-editor.gp-glass')));

    const firstDelete = deleteWrites.at(-1);
    button("Retry deletion",document).click(); await settle(120);
    check("malformed delete recovery uses the exact receipt and advances once", deleteWrites.at(-1).request_id === firstDelete.request_id && deleteWrites.length === 2);
    await settle(300);
    check("confirmed deletion removes the task and its open sheet", !card("task-10") && !editor());

    const menu = () => document.querySelector('[role="menu"][aria-label="Task actions"]');
    const openMenu = async id => { card(id).dispatchEvent(new MouseEvent("contextmenu",{bubbles:true,cancelable:true,clientX:innerWidth-1,clientY:innerHeight-1})); await settle(); };
    await openMenu("task-11");
    await settle(220); const rect=menu().getBoundingClientRect();
    check("task menus fit the bottom-right viewport boundary", rect.right <= innerWidth && rect.bottom <= innerHeight && rect.left >= 0 && rect.top >= 0);
    // Fitting is only half the claim: a menu that never got positioned at all
    // sits at the origin, which also "fits". The anchor was the bottom-right
    // corner, so a menu that was actually placed is flush against it, one
    // 8px inset away. This is what proves the shared popover owner's inline
    // left/top survives to paint rather than being overwritten.
    check("task menus are clamped to the anchor, not left at the origin", Math.abs(rect.right - (innerWidth - 8)) <= 1 && Math.abs(rect.bottom - (innerHeight - 8)) <= 1);
    menu().dispatchEvent(new KeyboardEvent("keydown",{key:"End",bubbles:true,cancelable:true})); await settle();
    check("End selects the last task menu action", document.activeElement.textContent.includes("Delete"));
    menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
    check("Escape dismisses the portaled task menu", !menu());
    card("task-11").focus(); card("task-11").dispatchEvent(new KeyboardEvent("keydown",{key:"F10",shiftKey:true,bubbles:true,cancelable:true})); await settle();
    check("Shift F10 opens the focused task actions", Boolean(menu()));
    button("Move to…",menu()).click(); await settle(); button("Backlog",menu()).click(); await settle(150);
    check("context-menu status changes persist without replacing task details", tasks.find(task=>task.id==="task-11").status === "backlog");
    await openMenu("task-11"); button("Set priority…",menu()).click(); await settle(); button("Urgent",menu()).click(); await settle(150);
    check("context-menu priority choices persist", tasks.find(task=>task.id==="task-11").priority === 0);
    await openMenu("task-11"); button("Duplicate task…",menu()).click(); await wait(editor);
    check("duplicate opens an unsaved copy with preserved evidence and links", field("Title").value === tasks.find(task=>task.id==="task-11").title + " (copy)" && field("Description").value === tasks.find(task=>task.id==="task-11").description && field("Status").value === tasks.find(task=>task.id==="task-11").status);
    confirmAnswer=true; await click("Close task details");

    const selectCard=async id=>{ card(id).dispatchEvent(new MouseEvent("click",{bubbles:true,ctrlKey:true})); await settle(); };
    if(root.querySelector(".selection")) await click("Clear task selection");
    await selectCard("task-12"); await selectCard("task-13");
    check("modifier selection exposes the exact batch size", root.querySelector('[aria-label="Selected task actions"]').textContent.includes("2 selected"));
    await click("Delete selected tasks");
    let dialog=document.querySelector('[aria-label="Delete tasks"]');
    check("bulk deletion confirms every selected task and cross-scope effects", dialog.querySelectorAll("li").length===2 && dialog.textContent.includes("every linked repository and workspace") && document.activeElement.textContent === "Cancel");
    button("Cancel",dialog).click(); await settle();
    check("cancelled deletion does not issue storage writes", deleteWrites.length===2 && !deleted.has("task-12"));
    try {
      // A confirmed deletion is held, not sent: the store never reuses a deleted
      // id, so not sending it yet is the only undo a deletion can have.
      const undoStrip = () => root.querySelector('[data-testid="task-undo"]');
      const deleteDialog = () => document.querySelector('[aria-label="Delete tasks"]');
      await click("Delete selected tasks"); dialog=deleteDialog();
      check("the deletion confirmation says how long Undo keeps the tasks", dialog?.textContent.includes("12 seconds"));
      button("Delete 2 tasks",dialog).click(); await settle();
      check("a confirmed deletion leaves the board at once and writes nothing yet",
        !card("task-12") && !card("task-13") && deleteWrites.length===2 && !deleteDialog() && Boolean(undoStrip()?.textContent.includes("Deleted 2 tasks")));
      button("Undo", undoStrip()).click(); await settle();
      check("Undo inside the window brings the tasks back and never sent a deletion",
        Boolean(card("task-12")) && Boolean(card("task-13")) && deleteWrites.length===2 && !deleted.has("task-12") && !undoStrip());
      await selectCard("task-12"); await selectCard("task-13");
      await click("Delete selected tasks"); button("Delete 2 tasks",deleteDialog()).click(); await settle();
      card("task-14").dispatchEvent(new KeyboardEvent("keydown",{key:"z",metaKey:true,ctrlKey:true,bubbles:true,cancelable:true})); await settle();
      check("Command- or Control-Z undoes a pending deletion from the keyboard", Boolean(card("task-12")) && Boolean(card("task-13")) && deleteWrites.length===2);
      // Delete now sends it, with the same receipts and partial-failure report
      // every other task action has.
      await selectCard("task-12"); await selectCard("task-13");
      await click("Delete selected tasks"); button("Delete 2 tasks",deleteDialog()).click(); await settle();
      tasks.find(task=>task.id==="task-13").revision++; holdDelete=true;
      button("Delete now", undoStrip()).click(); await wait(()=>releaseDelete);
      check("a deletion being written can no longer be undone or sent twice",
        button("Undo", undoStrip())?.disabled === true && button("Delete now", undoStrip())?.disabled === true && !card("task-12"));
      holdDelete=false; releaseDelete(); await wait(()=>deleteDialog()?.textContent.includes("1 of 2 deleted"));
      dialog=deleteDialog();
      check("mixed batch deletion preserves concurrent edits and reports partial failure", deleted.has("task-12") && !deleted.has("task-13") && dialog.textContent.includes("1 not changed"));
      await wait(()=>card("task-13"));
      check("the task that was not deleted is back on the board", Boolean(card("task-13")) && !card("task-12") && !undoStrip());
      button("Done",dialog).click(); await settle(); if (root.querySelector(".selection")) await click("Clear task selection");
      // A lost reply leaves the same exact retry the dialog always offered.
      await selectCard("task-13"); await click("Delete selected tasks"); button("Delete 1 task",deleteDialog()).click(); await settle();
      loseDelete = true; button("Delete now", undoStrip()).click(); await wait(()=>button("Retry deletion", deleteDialog() ?? document));
      const lostDelete = deleteWrites.at(-1);
      check("an uncertain deferred deletion opens the dialog with its exact retry, and it cannot be closed",
        Boolean(button("Retry deletion", deleteDialog())) && button("Close task action", deleteDialog()).disabled);
      button("Retry deletion", deleteDialog()).click(); await wait(()=>deleteDialog()?.textContent.includes("1 of 1 deleted"));
      check("the retry reuses the lost request and deletes once", deleteWrites.at(-1).request_id === lostDelete.request_id && deleted.has("task-13"));
      button("Done", deleteDialog()).click(); await settle();
      // And with nobody pressing anything, the window closes on its own. The
      // fixture compresses the clock for this one timer — the board's 12 s
      // window — so the run does not spend twelve real seconds waiting on it.
      {
        const realSetTimeout = window.setTimeout;
        window.setTimeout = (fn, ms, ...rest) => realSetTimeout(fn, ms === 12_000 ? 1_200 : ms, ...rest);
        try {
          await selectCard("task-30"); await click("Delete selected tasks"); button("Delete 1 task",deleteDialog()).click(); await settle();
        } finally { window.setTimeout = realSetTimeout; }
        const writesBeforeWindow = deleteWrites.length;
        await settle(400);
        check("nothing is sent while the window is open", deleteWrites.length === writesBeforeWindow && !card("task-30"));
        const windowDeadline = Date.now() + 5_000;
        while (Date.now() < windowDeadline && !deleted.has("task-30")) await settle(100);
        check("the deletion is sent once the window closes", deleted.has("task-30") && deleteWrites.length === writesBeforeWindow + 1 && !undoStrip());
      }
    } catch (error) { check(`the deferred deletion checks ran to the end (${error.message})`, false); }

    await selectCard("task-14"); await selectCard("task-15");
    await openMenu("task-14"); button("Move to…",menu()).click(); await settle(); button("Review",menu()).click(); await wait(()=>tasks.find(task=>task.id==="task-15").status==="review"); await settle();
    check("bulk organization preserves both task briefs", ["task-14","task-15"].every(id=>tasks.find(task=>task.id===id).status==="review" && tasks.find(task=>task.id===id).description.includes("Keep changes focused")));
    await click("List view");
    check("list layout reuses the same task data", Boolean(root.querySelector('[aria-label="Task list"]')) && Boolean(card("task-14")));
    await click("Filters"); await change(root.querySelector('[aria-label="Filter by priority"]'),"1","change");
    check("priority filtering only shows matching loaded tasks", [...root.querySelectorAll('[data-task-card]')].every(el=>tasks.find(task=>task.id===el.dataset.cardId).priority===1) && root.querySelectorAll("[data-task-card]").length>0);
    await change(root.querySelector('[aria-label="Filter by priority"]'),"0","change"); await change(root.querySelector('[aria-label="Filter by label"]'),"usability","change");
    check("filtered empty states explain how to recover", root.textContent.includes("No tasks match") && Boolean(button("Clear filters")));
    await click("Clear filters"); await click("Board view");
    await selectCard("task-16"); await change(root.querySelector('[aria-label="Search tasks"]'),"Ready task"); await settle(400);
    check("changing search clears hidden selections", !root.querySelector('[aria-label="Selected task actions"]'));
    await click("Clear task search"); await wait(()=>card("task-1"));

    failConfiguration=true;
    await openMenu("task-2"); button("Quick Enhance",menu()).click(); await wait(()=>button("Close Quick Enhance",document)); await settle(100);
    check("Quick Enhance surfaces saved description and hidden task context", document.querySelector('[aria-labelledby="quick-enhance-title"]').textContent.includes("GitPulse") && document.querySelector('[aria-labelledby="quick-enhance-title"]').textContent.includes("Manvi") && document.querySelector('[aria-labelledby="quick-enhance-title"]').textContent.includes("Bharath"));
    button("Open full editor",document).click(); await wait(editor); await wait(()=>!document.querySelector('[aria-labelledby="quick-enhance-title"]')); await settle();
    check("Manvi configuration failure has an explicit retry", Boolean(button("Retry Manvi configuration")));
    failConfiguration=false; await click("Retry Manvi configuration");
    await wait(() => { const el = enhanceButton(); return Boolean(el) && !el.matches(":disabled"); });
    await click("Improve with Manvi"); await wait(()=>[...proposals.values()].some(p=>p.state==="running"));
    check("Improve with Manvi prevents duplicate generations while work is live", enhanceButton().matches(":disabled"));
    const proposal=[...proposals.values()].at(-1);
    proposals.set(proposal.id,{...proposal,revision:proposal.revision+1,state:"ready",proposed:{title:"A focused task brief",description:"Test suggestion with a verifiable outcome."},rationale:"Make the intended outcome explicit."});
    await wait(()=>Boolean(button("Use both")) || Boolean(button("Accept selected fields")) || Boolean(button("Use this title")));
    const originalTask=structuredClone(tasks.find(task=>task.id==="task-2"));
    const review=editor().querySelector('[aria-label="Enhancement review"]');
    if(review) {
      const descriptionCheck=[...review.querySelectorAll('label')].find(label=>label.textContent.trim()==="Description")?.querySelector('input');
      if(descriptionCheck?.checked) { descriptionCheck.click(); await settle(); }
    }
    // Choosing which fields to accept must not mark the task edited: the sheet
    // marks dirty from any change inside its form, and the unsaved-edits guard
    // would then refuse the acceptance this checkbox was selecting.
    check("choosing which fields to accept is not an edit to the task",
      Boolean(button("Accept selected fields")) && !button("Accept selected fields").disabled
      && !(editor()?.textContent ?? "").includes("before accepting a suggestion"));
    loseEnhancement=true;
    if(button("Accept selected fields")) await click("Accept selected fields");
    else if(button("Use this title")) await click("Use this title");
    else await click("Use both");
    await wait(()=>button("Retry pending action"));
    check("uncertain enhancement acceptance blocks editor close", button("Close task details").disabled);
    await click("Retry pending action"); await wait(()=>button("Undo accepted fields") || tasks.find(task=>task.id==="task-2").title==="A focused task brief");
    if(button("Undo accepted fields")) {
      check("Quick Enhance acceptance retries once and preserves unselected content", tasks.find(task=>task.id==="task-2").revision===originalTask.revision+1 && tasks.find(task=>task.id==="task-2").description===originalTask.description && enhancementWrites.filter(w=>w.method==="enhancements.accept").at(-1).request_id===enhancementWrites.filter(w=>w.method==="enhancements.accept")[0].request_id);
      await click("Undo accepted fields"); await settle(100);
      check("Quick Enhance undo restores accepted fields", tasks.find(task=>task.id==="task-2").title===originalTask.title);
    } else {
      check("inline Manvi acceptance retries once", tasks.find(task=>task.id==="task-2").title==="A focused task brief" && enhancementWrites.filter(w=>w.method==="enhancements.accept").length>=1);
    }
    // ---- Picking a past suggestion ---------------------------------------
    // The list used to be buttons reading `state · model` + `Task revision N`
    // inside a collapsed drawer: two runs of one model against one revision
    // were the same string twice, and the review the selection drove was
    // suppressed whenever a ready suggestion was showing beside the fields.
    const picker = () => editor()?.querySelector('[data-testid="task-assist-history"]');
    const options = () => [...(picker()?.options ?? [])];
    const reviewArticle = () => editor()?.querySelector('article[aria-label="Enhancement review"]');
    const reviewText = () => reviewArticle()?.textContent ?? "";
    const liveProposal = [...proposals.values()].filter(entry => entry.task_id === "task-2").at(-1);
    if (liveProposal) {
      // A second attempt: same task, same revision, same model, and stamped in
      // the same second as the first. Only the picker's own ordinal can tell
      // these apart, which is exactly the case the old label could not.
      const twin = {
        ...structuredClone(liveProposal),
        id: "enhancement-twin",
        revision: 1,
        state: "ready",
        proposed: { title: "A second focused brief", description: "A different wording of the same task." },
        rationale: "An alternative phrasing.",
      };
      proposals.set(twin.id, twin);
      const refresh = button("Refresh", editor());
      if (refresh) { refresh.click(); await settle(250); }
      check("every attempt is listed, with enough on each row to tell them apart",
        Boolean(picker()) && options().length >= 2
        && new Set(options().map(option => option.textContent.trim())).size === options().length
        && options().every(option => /^#\d+ · /.test(option.textContent.trim())));
      check("the picker reports the page it loaded rather than implying it is complete",
        (editor()?.textContent ?? "").includes("Showing " + options().length + " of "));
      const before = reviewText();
      const other = options().find(option => option.value !== picker().value);
      if (other) {
        picker().value = other.value;
        picker().dispatchEvent(new Event("change", { bubbles: true }));
        await settle(250);
      }
      // The point of the whole control: changing it always changes what is on
      // screen. Against the old code there was no review element at all here.
      check("changing the selection shows that suggestion, every time",
        Boolean(other) && Boolean(reviewArticle()) && reviewText() !== before
        && picker().value === other.value);
      check("the picker never names a suggestion the review is not rendering",
        Boolean(reviewArticle()) && Boolean(picker())
        && options().some(option => option.value === picker().value));
    } else {
      check("every attempt is listed, with enough on each row to tell them apart", false);
      check("the picker reports the page it loaded rather than implying it is complete", false);
      check("changing the selection shows that suggestion, every time", false);
      check("the picker never names a suggestion the review is not rendering", false);
    }

    // Due is a picker now, not a `datetime-local` box: a trigger, a portaled
    // popover, and a word box that shares quick add's `due:` grammar. Driving
    // it the way a reader does is also what proves the grammar is wired —
    // typing the date and pressing Return is the whole interaction.
    dueTrigger().click(); await settle(80);
    const duePhrase = document.querySelector('[data-testid="task-due-phrase"]');
    await change(duePhrase, "2026-09-01 09:30");
    duePhrase.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle(120);
    const dueExpected = Math.floor(new Date(2026, 8, 1, 9, 30, 0, 0).getTime() / 1000);
    check("the due picker reads a typed date through quick add's own grammar",
      dueTrigger().textContent.includes("2026") && !dueTrigger().textContent.includes("No due date"));
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })); await settle(60);
    await click("Save task"); await settle(100);
    // The stored second, not the rendered string: the trigger formats for the
    // reader's locale, so asserting its text would only test the runner's.
    check("due dates round-trip through the task editor",
      tasks.some(task => task.id === "task-2" && task.due_at === dueExpected)
      && dueTrigger().textContent.includes("2026"));
    editor().querySelector('[data-sheet-tab="agent"]').click(); await settle();
    await change(field("Owner"),"ada"); await click("Save task"); await settle(100);
    check("saving keeps the reader on the pane they were editing", editor().querySelector('[data-sheet-tab="agent"]').getAttribute("aria-selected")==="true");
    editor().querySelector('[data-sheet-tab="task"]').click(); await settle();
    await change(field("Description"),"Unsaved context");
    check("unsaved edits stay local until Manvi prepare saves", editor().querySelector("header small").textContent==="Unsaved" && Boolean(enhanceButton()) && !enhanceButton().matches(":disabled"));
    confirmAnswer=true; await click("Close task details");
    await click("New task");
    const idea=root.querySelector(".notes-label textarea");
    // "Draft", not "Improve": an empty new task has no notes, title or
    // description, so `draftingKind` is `draft` and the verb follows the kind.
    // This line pinned "Improve" — the exact disagreement between the button's
    // verb and the request's kind that `draftingKind`/`draftingVerb` were
    // extracted to end, so it was asserting the bug rather than the fix.
    check("new tasks start with one idea field and the current repository", Boolean(idea) && idea.getClientRects().length>0 && Boolean(button("Draft with Manvi")) && !editor().querySelector('.sheet-tabs'));
    check("a new task does not fold its notes",
      !assistToggle() && assistBody() && !assistBody().hidden
      && (idea.compareDocumentPosition(field("Title")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0);
    await change(idea,"Fix notification routing\nKeep saved task evidence and explain recovery steps.");
    await wait(() => button("Draft with Manvi") && !button("Draft with Manvi").disabled);
    await click("Draft with Manvi");
    await wait(()=>[...proposals.values()].some(p=>p.source.title==="Fix notification routing" && p.state==="running"));
    const quickProposal=[...proposals.values()].find(p=>p.source.title==="Fix notification routing");
    check("inline drafting refuses duplicate generations while a worker is running", enhanceButton().matches(":disabled"));
    const assistBox = editor().querySelector('[aria-label="Manvi task assist"]');
    // The intent has always been "the assist is not buried below every field".
    // A two-column layout keeps that promise sideways, so the old vertical
    // `stacked()` test no longer expresses it: assert it is reachable without
    // scrolling instead.
    const bodyBox = () => editor().querySelector(".sheet-body").getBoundingClientRect();
    check("the assist is reachable without scrolling past the task",
      Boolean(assistBox) && assistBox.getClientRects().length > 0
      && assistBox.getBoundingClientRect().top < bodyBox().bottom);
    check("one click saves the rough idea and starts Manvi with both fields", quickProposal.fields.length===2 && quickProposal.source.description.includes("explain recovery steps") && quickProposal.source.repository_ids[0]==="repo-0");
    check("Draft with Manvi consumes notes after extract", idea.value.trim() === "");
    check("drafted sheet keeps a single Manvi assist section", Boolean(editor().querySelector('[aria-label="Manvi task assist"]')) && !editor().querySelector('[aria-label="Manvi task enhancements"]'));
    // Acceptance has exactly one home now: the review, beside the diff. The
    // sheet used to draw "Use this title" under each field while the review
    // drew no buttons at all, which is what let the history picker change
    // nothing visible. A vacuous version of this check passed either way, so
    // it asserts both halves: the review offers it, and the sheet does not.
    const acceptButton = () => button("Accept selected fields") ?? button("Apply enhancement");
    proposals.set(quickProposal.id,{...quickProposal,revision:quickProposal.revision+1,state:"ready",proposed:{title:"Reliable task notifications",description:"Preserve saved evidence, route notifications to the correct task, and verify recovery."}});
    await wait(()=>acceptButton());
    // Asserted once the suggestion is ready, which is the only state in which
    // acceptance is offered at all.
    check("acceptance lives once, in the review that shows the diff",
      (() => {
        const article = editor().querySelector('article[aria-label="Enhancement review"]');
        if (!article) return false;
        if (button("Use both") || button("Use this title") || button("Use this description")) return false;
        return Boolean(acceptButton()) && article.contains(acceptButton())
          && article.querySelectorAll('input[type="checkbox"]').length > 0;
      })());
    await change(field("Description"),"Unsaved evidence that must survive");
    check("acceptance refuses to overwrite unsaved task edits, and says why",
      acceptButton().matches(":disabled")
      && editor().textContent.includes("before accepting a suggestion"));
    await change(field("Description"),quickProposal.source.description);
    // Reload the saved revision so this acceptance starts from clean state.
    await click("Reload saved"); await settle(100);
    loseEnhancement=true; acceptButton().click(); await settle();
    const retryInline=button("Retry pending action");
    check("a lost acceptance retains a retry and blocks closing", Boolean(retryInline) && button("Close task details").disabled);
    if(retryInline) await click("Retry pending action");
    await wait(()=>tasks.find(task=>task.id===quickProposal.task_id)?.title==="Reliable task notifications");
    check("applying the default quick enhancement updates title and description together", tasks.find(task=>task.id===quickProposal.task_id).description.includes("verify recovery"));
    await click("Close task details");
    await click("New task");
    await change(root.querySelector(".notes-label textarea"),"Prepare Demo for Seattle start-up event");
    await click("Save task"); await settle(100);
    check("notes-only task saves without native title validation blocking extraction", tasks.some(task=>task.title==="Prepare Demo for Seattle start-up event"));
    const saveBar=editor().querySelector(".task-editor > footer") ?? editor().querySelector("footer");
    const footerBackground=getComputedStyle(saveBar).backgroundColor;
    const footerAlpha=footerBackground === "transparent" ? 0 : footerBackground.startsWith("rgba") && !footerBackground.endsWith(", 1)") ? 0 : 1;
    check("save bar is not an opaque slab", saveBar.parentElement === editor() && footerAlpha < 1);
    confirmAnswer=true; await click("Close task details");
    failConfiguration=true; await click("New task"); await wait(()=>editor().textContent.includes("Manvi temporarily unavailable"));
    check("inline configuration failure exposes a working retry", Boolean(button("Retry Manvi configuration")) && !button("Retry Manvi configuration").matches(":disabled"));
    failConfiguration=false; blankConfiguration=true; await click("Retry Manvi configuration");
    check("an unconfigured model exposes selection beside drafting", Boolean(editor().querySelector(".manvi-assist .change-link")) && enhanceButton().matches(":disabled"));
    blankConfiguration=false;
    await harnessStore.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "replacement-fixture" });
    await settle(150);
    await change(editor().querySelector(".notes-label textarea"),"A recoverable task");
    check("choosing a task model recovers drafting without reopening the editor", !button("Draft with Manvi").matches(":disabled"));
    editor().querySelectorAll(".field-picks button").forEach(el=>el.click()); await settle();
    const emptySelectionWrites=enhancementWrites.length;
    editor().querySelector(".notes-label textarea").dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",ctrlKey:true,bubbles:true})); await settle();
    check("empty field selection cannot widen through the keyboard shortcut", button("Draft with Manvi").matches(":disabled") && enhancementWrites.length===emptySelectionWrites);
    blankConfiguration=false; await click("Close task details");

    // ---- One-line entry -------------------------------------------------
    // Adding a task used to mean opening a sheet with twenty controls. This
    // is the LiquiTask shape: one line, parsed as you type, Enter commits.
    const quickAdd = () => root.querySelector('[data-testid="task-quick-add"] input');
    const quickPreview = () => root.querySelector('[data-testid="task-quick-add-preview"]');
    const chips = () => [...(quickPreview()?.querySelectorAll(".chip") ?? [])].map(el => el.textContent.trim());
    const enter = (el, extra = {}) => { el.dispatchEvent(new KeyboardEvent("keydown", {key:"Enter", bubbles:true, cancelable:true, ...extra})); };
    quickAdd().focus();
    await change(quickAdd(), "Ship the release notes !high #docs #release @ada ~feature :: with the changelog");
    check("quick add parses markers into fields while typing",
      chips().includes("High") && chips().includes("feature") && chips().includes("ada")
      && chips().includes("docs") && chips().includes("release") && chips().includes("notes")
      && chips().includes("Ship the release notes"));
    check("quick add keeps markers out of the title", !chips()[0].includes("!high") && !chips()[0].includes("#docs"));
    const beforeQuick = tasks.length;
    enter(quickAdd()); await settle(250);
    const added = tasks.find(task => task.title === "Ship the release notes");
    check("quick add creates the task the preview promised",
      tasks.length === beforeQuick + 1 && Boolean(added) && added.priority === 1 && added.kind === "feature"
      && added.owner === "ada" && [...added.labels].sort().join(",") === "docs,release"
      && added.description === "with the changelog" && quickAdd().value === "");
    await change(quickAdd(), "#orphan @nobody !urgent");
    check("markers with no title refuse to become a task",
      quickPreview().textContent.includes("Markers alone do not make a task")
      && button("Add", root.querySelector('[data-testid="task-quick-add"]')).disabled);
    enter(quickAdd()); await settle(150);
    check("a refused quick add writes nothing", tasks.length === beforeQuick + 1);
    await change(quickAdd(), "Investigate the flake !2 #ci");
    enter(quickAdd(), {shiftKey:true}); await wait(editor);
    check("shift-enter hands the parsed line to the editor instead of saving",
      field("Title").value === "Investigate the flake" && tasks.length === beforeQuick + 1
      && !editor().querySelector(".sheet-tabs"));
    // A draft used to fold these behind a "Schedule and labels" switch. Two
    // columns give it room, so what the line filled in is simply on screen.
    check("an expanded quick add shows the fields it already filled in",
      field("Priority")?.value === "2"
      && Boolean(field("Labels")) && field("Labels").getClientRects().length > 0
      && editor().textContent.includes("ci"));
    confirmAnswer = true; await click("Close task details"); await settle();
    // A key reaches the board through whatever holds focus. This focused a
    // column, which is not focusable, and dispatched on `document`, which the
    // board's handler never accepts — so it passed only when focus happened to
    // be in quick add already, and never exercised the shortcut.
    const boardCard = root.querySelector("[data-task-card]");
    boardCard?.focus();
    const startedElsewhere = Boolean(boardCard) && document.activeElement === boardCard;
    boardCard?.dispatchEvent(new KeyboardEvent("keydown", {key:"a", bubbles:true}));
    await settle();
    check("the a shortcut puts the cursor in quick add", startedElsewhere && document.activeElement === quickAdd());
    await change(quickAdd(), "");

    // ---- Quick add, with the model ---------------------------------------
    // Drafting saves exactly what the manual mode saves, then asks for a title
    // and description to review. It never invents a task, and it never writes
    // before it has asked whatever it needs to ask.
    const modeGroup = () => root.querySelector('[aria-label="What Return does with this line"]');
    const modeButton = label => [...(modeGroup()?.querySelectorAll("button") ?? [])].find(el => el.textContent.trim() === label);
    const planLine = () => root.querySelector('[data-testid="task-quick-add-plan"]');
    const enhanceSheet = () => document.querySelector('[aria-labelledby="quick-enhance-title"]');
    check("quick add offers a drafting mode, off until it is chosen",
      Boolean(modeGroup()) && Boolean(modeButton("Manual")) && Boolean(modeButton("Draft"))
      && modeButton("Manual")?.getAttribute("aria-pressed") === "true"
      && modeButton("Draft")?.getAttribute("aria-pressed") === "false"
      && !planLine());
    quickAdd().focus();
    await change(quickAdd(), "Ship the second release notes !high #docs @ada ~feature");
    const manualChips = chips().join("|");
    modeButton("Draft")?.click(); await settle(80);
    quickAdd().focus(); await change(quickAdd(), "Ship the second release notes !high #docs @ada ~feature");
    check("the drafting mode changes nothing about what the line means",
      chips().join("|") === manualChips && modeButton("Draft")?.getAttribute("aria-pressed") === "true");
    const beforeDraftTasks = tasks.length;
    const beforeDraftWrites = enhancementWrites.length;
    check("the drafting mode says what it will write, before it writes anything",
      Boolean(planLine()) && planLine().textContent.includes("Saves this line now")
      && tasks.length === beforeDraftTasks && enhancementWrites.length === beforeDraftWrites);
    enter(quickAdd()); await settle(400);
    const draftedTask = tasks.find(task => task.title === "Ship the second release notes");
    check("a drafted quick add saves the typed line first, exactly as the manual mode would",
      Boolean(draftedTask) && draftedTask.priority === 1 && draftedTask.kind === "feature" && draftedTask.owner === "ada"
      && [...draftedTask.labels].join(",") === "docs");
    // The card on the board carries the reader's own words. There is no window
    // in which a placeholder title exists, because a line with no title is
    // refused in this mode exactly as it is in the other.
    check("the drafted card carries the typed title, never a placeholder",
      Boolean(card(draftedTask?.id)) && card(draftedTask.id).textContent.includes("Ship the second release notes"));
    await wait(() => Boolean(enhanceSheet()));
    check("a drafted quick add opens the review surface and starts the model there",
      Boolean(enhanceSheet())
      && enhancementWrites.some(write => write.method === "enhancements.create" && write.task_id === draftedTask?.id));
    // `EnhancementField` is title|description, so the marker fields are out of
    // scope by type rather than by a rule applied afterwards.
    const draftCreate = enhancementWrites.filter(write => write.method === "enhancements.create" && write.task_id === draftedTask?.id).at(-1);
    check("the model is never asked for a field the markers own",
      Boolean(draftCreate) && [...(draftCreate.fields ?? [])].sort().join(",") === "description,title");
    const closeEnhance = () => [...(enhanceSheet()?.querySelectorAll("button") ?? [])].find(el => el.getAttribute("aria-label") === "Close Quick Enhance");
    closeEnhance()?.click(); await settle(200);
    check("closing the review leaves the drafted task on the board", Boolean(card(draftedTask?.id)) && !enhanceSheet());

    // Opening the sheet to read a task is not a request for one. The drafting
    // counter only rises, and every fresh sheet starts from zero, so a value
    // left standing from the run above would auto-start on whatever is opened
    // next — spending a model request nobody asked for. A task made a moment
    // ago is the honest subject here: no earlier attempt and no field lock can
    // make the refusal happen for some other reason.
    modeButton("Manual")?.click(); await settle(60);
    quickAdd().focus(); await change(quickAdd(), "Read this one without drafting");
    enter(quickAdd()); await settle(300);
    const readOnly = tasks.find(task => task.title === "Read this one without drafting");
    const beforeReadWrites = enhancementWrites.length;
    confirmAnswer = true;
    if (readOnly) { await openMenu(readOnly.id); button("Quick Enhance", menu())?.click(); await wait(() => Boolean(enhanceSheet())); await settle(400); }
    check("opening Quick Enhance to read a task never starts a run of its own",
      Boolean(readOnly) && Boolean(enhanceSheet()) && enhancementWrites.length === beforeReadWrites);

    // A different task is a different sheet. It loads its task once, on mount,
    // so swapping the id under a live instance would leave the reader reading
    // the previous task while the drafting request is aimed at the new one.
    modeButton("Draft")?.click(); await settle(60);
    quickAdd().focus(); await change(quickAdd(), "Rotate the signing key");
    enter(quickAdd()); await settle(500);
    const swapped = tasks.find(task => task.title === "Rotate the signing key");
    await wait(() => Boolean(enhanceSheet()?.textContent.includes("Rotate the signing key"))).catch(() => {});
    check("drafting while a review is open moves the review onto the new task",
      Boolean(swapped) && Boolean(enhanceSheet()?.textContent.includes("Rotate the signing key"))
      && !enhanceSheet()?.textContent.includes("Read this one without drafting")
      && enhancementWrites.some(write => write.method === "enhancements.create" && write.task_id === swapped?.id));
    closeEnhance()?.click(); await settle(200);
    modeButton("Manual")?.click(); await settle(60);
    await change(quickAdd(), "");

    // ---- Board customization, and what it costs --------------------------
    const viewToggle = () => root.querySelector("[data-task-view-toggle]");
    const viewMenu = () => root.querySelector('[data-testid="task-view-menu"]');
    const columnToggle = status => viewMenu().querySelector(`[data-task-column-toggle="${status}"]`);
    const hiddenNote = () => root.querySelector('[data-testid="task-hidden-columns"]');
    const columnEl = status => root.querySelector(`[data-task-column="${status}"]`);
    viewToggle().click(); await settle();
    check("the view menu offers every column and card field",
      [...viewMenu().querySelectorAll("[data-task-column-toggle]")].length === 6
      && [...viewMenu().querySelectorAll("[data-task-field-toggle]")].length === 5);
    const backlogCount = root.querySelectorAll('[data-task-column="backlog"] [data-task-card]').length;
    columnToggle("backlog").click(); await settle();
    check("hiding a column removes it from the board",
      !columnEl("backlog") && columnToggle("backlog").getAttribute("aria-checked") === "false");
    // The count is asserted exactly, not as "contains a digit": the banner
    // exists to say how much work is out of sight, and a wrong number there is
    // the failure it was written to prevent. `backlogCount > 0` is asserted
    // rather than used to skip, so a fixture that stopped seeding Backlog turns
    // this red instead of quietly passing on the empty branch.
    const hiddenCount = Number(hiddenNote()?.textContent.match(/\d+/)?.[0]);
    check("a hidden column reports the work it is hiding, with an exact count",
      Boolean(hiddenNote()) && hiddenNote().textContent.includes("hidden from this board")
      && backlogCount > 0 && hiddenCount === backlogCount);
    for (const status of ["inbox","ready","in_progress","review","done"]) { columnToggle(status).click(); await settle(); }
    // `>= 1` passed whether the toggle refused or reset every column back on,
    // which is why the reset survived: exactly one column may still stand, and
    // the control that would remove it has to be visibly closed, not merely inert.
    check("the board refuses to hide its last column, and closes that control",
      [...viewMenu().querySelectorAll('[data-task-column-toggle][aria-checked="true"]')].length === 1
      && columnToggle("done").disabled === true
      && columnToggle("done").getAttribute("aria-checked") === "true"
      && Boolean(columnToggle("done").title)
      && root.querySelectorAll("[data-task-column]").length >= 1);
    check("every other column is still free to come back",
      ["inbox","backlog","ready","in_progress","review"].every(status => columnToggle(status).disabled === false));
    viewMenu().querySelector('[data-task-field-toggle="repo"]').click(); await settle();
    check("card fields are a choice the card obeys",
      viewMenu().querySelector('[data-task-field-toggle="repo"]').getAttribute("aria-checked") === "false");
    button("Reset board view", viewMenu()).click(); await settle();
    check("reset restores every column and field",
      root.querySelectorAll("[data-task-column]").length === 6 && !hiddenNote()
      && [...viewMenu().querySelectorAll('[data-task-field-toggle][aria-checked="true"]')].length === 5);
    viewToggle().click(); await settle();
    check("the view menu closes without leaving the board changed", !viewMenu() && root.querySelectorAll("[data-task-column]").length === 6);

    // ---- The archive, and what it is willing to claim --------------------
    const archive = () => root.querySelector('[data-testid="task-archive"]');
    const archiveRows = () => [...root.querySelectorAll('[data-testid="task-archive-row"]')];
    const archiveSummaryText = () => root.querySelector('[data-testid="task-archive-summary"]')?.textContent.replace(/\s+/g, " ").trim();
    const archiveToggle = () => [...root.querySelectorAll("header button")].find(el => el.getAttribute("aria-label") === "Archive");
    // Counted from the fixture rather than written as a literal: earlier
    // checks move tasks between columns and delete some, so a hard-coded
    // total would measure this block's position in the script, not the archive.
    const archivedCount = () => tasks.filter(task => !deleted.has(task.id) && task.archived).length;
    const expected = archivedCount();
    check("the board badges the server's archived total, not a page of it",
      expected > 30 && archiveToggle()?.textContent.trim() === String(expected));
    // dc-store schema 11: Done and archived are separate. Archived Done work
    // is off the board, and the Done column holds only what is not archived.
    check("archived work is not drawn in the Done column",
      tasks.filter(task => task.archived && !deleted.has(task.id)).every(task => !card(task.id)));
    archiveToggle().click(); await settle(200);
    await wait(() => archiveRows().length > 0);
    check("the archive opens on the archived tasks for this scope",
      Boolean(archive()) && archiveRows().length === 30);
    check("the archive lists the most recently completed first, stamped with it",
      archiveRows()[0].getAttribute("data-card-id") === [...tasks].filter(task => task.archived && !deleted.has(task.id)).sort((a, b) => (b.completed_at ?? 0) - (a.completed_at ?? 0))[0].id
      && archiveRows()[0].querySelector('[data-testid="task-archive-stamp"]')?.textContent.startsWith("Completed"));
    // The panel is named Archive and offers Restore. Until this line it never
    // said anywhere what puts a task in it, and the one place that came close
    // was the empty state — the single case a reader has no archived work to
    // ask about.
    check("the archive says how a task gets into it, with rows on screen",
      archive().querySelector('[data-testid="task-archive-rule"]')?.textContent.trim()
        === "Archive files a task away from the board in any column; Restore puts it back where it was.");
    // The one number this panel must not get wrong. 30 rows on screen out of
    // 34 archived tasks has to read as both numbers, or a reader clears an
    // archive they have only partly seen.
    check("a partly loaded archive says so, with both numbers",
      archiveSummaryText() === `Showing 30 of ${expected} archived tasks. Load more to see the rest.`);
    button("Load more", archive()).click(); await settle(200);
    check("Load more grows the page instead of replacing it",
      archiveRows().length === expected && archiveSummaryText() === `${expected} archived tasks.`);
    // A restore is the board's own update, so it must land in the board's
    // confirm dialog rather than writing straight through.
    archiveRows()[0].querySelector('input[type="checkbox"]').click(); await settle();
    archiveRows()[1].querySelector('input[type="checkbox"]').click(); await settle();
    // The defect this panel was reported for, measured rather than asserted
    // about the stylesheet. With the rows loaded, the Restore/Delete bar used
    // to render ~107px below the panel's own fold at scroll top: ticking a
    // checkbox changed nothing a reader could see, and there was no cue to
    // scroll. Both ends of the bar must be inside the panel's visible box,
    // and must stay there with the list scrolled to its end.
    const visibleIn = (el, within) => {
      const a = el.getBoundingClientRect(), b = within.getBoundingClientRect();
      return a.top >= b.top - 1 && a.bottom <= b.bottom + 1 && a.height > 0;
    };
    const actionBar = () => archive().querySelector(".actions");
    const entries = () => archive().querySelector(".entries");
    check("selecting a row shows the actions it enables, without scrolling first",
      Boolean(actionBar()) && visibleIn(actionBar(), archive())
      && actionBar().textContent.includes("Restore") && actionBar().textContent.includes("Delete"));
    entries().scrollTop = entries().scrollHeight; await settle(80);
    // Both are siblings of the scroller, never inside it: on macOS
    // `bg-surface` is a 50%-alpha material, and a row sliding under a
    // half-transparent bar puts a task title and that bar's own label in the
    // same pixels. Out here `.entries` clips its rows, so nothing can reach
    // them — which is a containment fact, not a geometric one. A scrolled-off
    // row keeps a rect that still intersects the bar's band while being
    // painted nowhere, so overlap is the wrong thing to measure.
    const listHead = () => archive().querySelector(".list-head");
    check("the actions and the select-all head stay put while the rows scroll",
      visibleIn(actionBar(), archive()) && visibleIn(listHead(), archive())
      && !entries().contains(listHead()) && !entries().contains(actionBar())
      && archiveRows().every(row => entries().contains(row)));
    // One scroller. Two nested ones with independent caps is what pushed the
    // action bar out: the row list won the panel's height and the actions
    // went below a fold the panel itself reported as absent.
    check("the panel scrolls its rows and nothing else",
      entries().scrollHeight > entries().clientHeight
      && archive().scrollHeight <= archive().clientHeight + 1);
    entries().scrollTop = 0; await settle(60);
    const restored = archiveRows().slice(0, 2).map(row => row.getAttribute("data-card-id"));
    const restoredRevisions = restored.map(id => tasks.find(task => task.id === id)?.revision);
    button("Restore", archive()).click(); await settle(150);
    const restoreDialog = document.querySelector('[role="dialog"][aria-label="Update tasks"]');
    check("restoring goes through the board's confirm-and-retry dialog", Boolean(restoreDialog));
    button("Update 2 tasks", restoreDialog).click(); await settle(250);
    button("Done", restoreDialog).click(); await settle(250);
    // Restore clears the flag and nothing else: a Done task comes back to
    // Done, which is the column it was archived from.
    check("a restored task leaves the archive and returns to its own column, status unchanged",
      restored.every(id => { const task = tasks.find(item => item.id === id); return task?.status === "done" && task.archived === false; })
      && restored.every((id, at) => tasks.find(task => task.id === id)?.revision === restoredRevisions[at] + 1)
      && archiveRows().every(row => !restored.includes(row.getAttribute("data-card-id")))
      && restored.every(id => Boolean(card(id)?.closest('[data-task-column="done"]'))));
    // The pages already loaded are re-read in place: the reader who scrolled
    // to the second page keeps both, against the new total.
    check("a restore refreshes the loaded pages in place, against the new total",
      archiveToggle().textContent.trim() === String(expected - 2)
      && archiveRows().length === expected - 2
      && archiveSummaryText() === `${expected - 2} archived tasks.`);
    await change(archive().querySelector('[aria-label="Search archived tasks"]'), "Completed task 30");
    await settle(350);
    check("the archive searches archived work without touching the board's search",
      archiveRows().length === 1 && root.querySelector('[aria-label="Search tasks"]').value === "");
    await change(archive().querySelector('[aria-label="Search archived tasks"]'), "");
    await settle(350);
    // ---- The Deleted view: restore a deleted task with its id ------------
    // One that was not archived, so restoring it leaves the archive's count alone.
    const gone = tasks.find(task => deleted.has(task.id) && !task.archived);
    if (!gone) throw Error("The Deleted view checks need a task an earlier block deleted");
    archive().querySelector('[data-testid="task-archive-view-deleted"]').click(); await settle(250);
    await wait(() => archiveRows().length > 0);
    check("the Deleted view lists deleted tasks, and only those",
      archiveRows().some(row => row.getAttribute("data-card-id") === gone.id)
      && archiveRows().every(row => deleted.has(row.getAttribute("data-card-id")))
      && archiveSummaryText().includes("deleted task"));
    const goneRow = archiveRows().find(row => row.getAttribute("data-card-id") === gone.id);
    goneRow.querySelector('input[type="checkbox"]').click(); await settle();
    check("the Deleted view offers Restore and no second Delete",
      Boolean(archive().querySelector('[data-testid="task-archive-restore"]')) && !button("Delete", archive().querySelector(".actions")));
    archive().querySelector('[data-testid="task-archive-restore"]').click(); await settle(400);
    check("restoring a deleted task brings the same id back, checked against its deletion's revision",
      !deleted.has(gone.id)
      && restoreWrites.some(write => write.id === gone.id && write.expected_revision === gone.revision - 1)
      && archiveRows().every(row => row.getAttribute("data-card-id") !== gone.id));
    archive().querySelector('[data-testid="task-archive-view-archived"]').click(); await settle(250);
    await wait(() => archiveRows().length > 0);
    // A backgrounded window defers the query, the way the Inbox does. What it
    // must not do is answer it: an unread archive saying "No archived tasks
    // in this scope" underneath a header badge reading 34 is a check that
    // could not run reporting the same result as one that ran and passed.
    archiveToggle().click(); await settle();
    const realHidden = Object.getOwnPropertyDescriptor(Document.prototype, "hidden");
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    document.dispatchEvent(new Event("visibilitychange"));
    archiveToggle().click(); await settle(250);
    check("a backgrounded window never reports an unread archive as an empty one",
      Boolean(archive())
      && archiveRows().length === 0
      && !archiveSummaryText().includes("No archived tasks")
      && archiveSummaryText().includes("have not loaded yet")
      && archiveSummaryText().includes("Paused while this window is in the background")
      && archiveToggle().textContent.trim() === String(expected - 2));
    Object.defineProperty(document, "hidden", realHidden);
    document.dispatchEvent(new Event("visibilitychange"));
    await wait(() => archiveRows().length > 0);
    check("returning to the foreground loads the archive without a manual refresh",
      archiveRows().length === Math.min(30, expected - 2) && !archiveSummaryText().includes("have not loaded yet"));

    archiveToggle().click(); await settle();
    check("the archive closes and leaves the board as it was",
      !archive() && root.querySelectorAll("[data-task-column]").length === 6);

    // ---- Archiving a task -------------------------------------------------
    // The report this block exists for: a panel called Archive, a Restore
    // inside it, and no Archive verb anywhere in the product. Since dc-store
    // schema 11 the verb sets the task's own flag; it no longer moves it.
    const archiveRow = () => [...menu().querySelectorAll("button")].find(el => el.dataset.menuId === "archive");
    const of = id => tasks.find(task => task.id === id);
    // Taken from the board as it stands, not written as literals: earlier
    // blocks in this script delete tasks and move others between columns, so
    // a hard-coded id would be measuring this block's position in the script.
    const onBoardIn = status => [...root.querySelectorAll(`[data-task-column="${status}"] [data-task-card]`)]
      .map(el => el.getAttribute("data-card-id"));
    // Archived cards leave the board, so never one a later block opens by id.
    const [first, second, third] = onBoardIn("ready").filter(id => !["task-11", "task-31", "task-32"].includes(id));
    if (!first || !second || !third) throw Error("Archive checks need three Ready cards on the board");
    await openMenu(first);
    check("a card's own menu offers Archive, and says it keeps the status",
      Boolean(archiveRow()) && !archiveRow().disabled
      && archiveRow().textContent.includes("Archive") && archiveRow().textContent.includes("Keeps status"));
    const beforeArchive = archivedCount(), firstRevision = of(first).revision;
    archiveRow().click(); await settle(350);
    check("Archive files a Ready task away without moving it to Done or opening a dialog",
      of(first).status === "ready" && of(first).archived === true && of(first).revision === firstRevision + 1
      && !document.querySelector('[role="dialog"]')
      && Number(archiveToggle().textContent.trim()) === beforeArchive + 1
      && !card(first));

    // The bulk half. An archived task is off the board, so the selection
    // cannot mix the two here; `taskMenu.test.ts` holds that case.
    if (root.querySelector(".selection")) await click("Clear task selection");
    await selectCard(second); await selectCard(third);
    const bulkButton = () => root.querySelector('[data-testid="task-archive-selected"]');
    const before = { second: of(second).revision, third: of(third).revision };
    check("the selection bar offers Archive beside Delete",
      Boolean(bulkButton()) && !bulkButton().disabled
      && root.querySelector('[aria-label="Selected task actions"]').textContent.includes("2 selected"));
    bulkButton().click(); await settle(450);
    check("bulk Archive files each task away once, keeping its status",
      [second, third].every(id => of(id).archived === true && of(id).status === "ready" && !card(id))
      && of(second).revision === before.second + 1 && of(third).revision === before.third + 1
      && Number(archiveToggle().textContent.trim()) === beforeArchive + 3);
    if (root.querySelector(".selection")) await click("Clear task selection");

    // ---- The fuller right-click menu ------------------------------------
    await openMenu("task-11");
    const labels = () => [...menu().querySelectorAll("button")].map(el => el.textContent.trim());
    const wanted = ["Move to…","Set priority…","Due…","Owner…","Labels…","Send to agent…","Copy…"];
    const missingRows = wanted.filter(name => !labels().some(text => text.includes(name)));
    check(`the menu offers schedule, owner, labels and an agent handoff${missingRows.length ? ` (missing ${missingRows.join(", ")} of ${labels().join(" / ")})` : ""}`, missingRows.length === 0);
    button("Set priority…", menu()).click(); await settle();
    const currentPriority = [...menu().querySelectorAll('[role="menuitemcheckbox"]')].find(el => el.getAttribute("aria-checked") === "true");
    check("a value submenu lists every choice and marks the current one",
      [...menu().querySelectorAll('[role="menuitemcheckbox"]')].length === 4
      && Boolean(currentPriority) && currentPriority.textContent.includes("Urgent") && currentPriority.disabled);
    menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
    await openMenu("task-11"); button("Due…", menu()).click(); await settle();
    button("Tomorrow", menu()).click(); await settle(200);
    check("the menu can schedule a task without opening it", Number.isFinite(tasks.find(task=>task.id==="task-11").due_at));
    await openMenu("task-11"); button("Labels…", menu()).click(); await settle();
    const firstLabel = [...menu().querySelectorAll('[role="menuitemcheckbox"]')][0];
    const labelText = firstLabel?.textContent.trim();
    firstLabel?.click(); await settle(200);
    check("the menu toggles a label in place", tasks.find(task=>task.id==="task-11").labels.includes(labelText));
    if (menu()) { menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle(); }

    // ---- Bulk edits from the selection bar, and undo ----------------------
    try {
      const undoStrip = () => root.querySelector('[data-testid="task-undo"]');
      const of = id => tasks.find(task => task.id === id);
      const pick = async id => { card(id).dispatchEvent(new MouseEvent("click",{bubbles:true,ctrlKey:true})); await settle(); };
      if (root.querySelector(".selection")) await click("Clear task selection");
      await pick("task-31"); await pick("task-32");
      root.querySelector('[data-testid="task-selection-change"]')?.click(); await settle();
      const rows = () => [...(menu()?.querySelectorAll("button") ?? [])].map(el => el.textContent.trim());
      check("the selection bar opens status, label, priority, owner and due changes for the whole selection",
        ["Move to…","Labels…","Set priority…","Owner…","Due…"].every(name => rows().some(text => text.includes(name))));
      const before = ["task-31","task-32"].map(id => ({ id, labels: [...of(id).labels], revision: of(id).revision }));
      button("Labels…", menu()).click(); await settle();
      const bulkRow = [...menu().querySelectorAll('[role="menuitemcheckbox"]')].find(el => !before.some(entry => entry.labels.includes(el.textContent.trim())));
      const bulkLabel = bulkRow?.textContent.trim();
      bulkRow?.click(); await wait(() => ["task-31","task-32"].every(id => of(id).labels.includes(bulkLabel)));
      if (menu()) { menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle(); }
      check("a bulk label lands on every selected task in one revision each", before.every(entry => of(entry.id).revision === entry.revision + 1));
      check("the board offers to undo the bulk change it just wrote, and names it", undoStrip()?.textContent.includes(`Added label “${bulkLabel}” to 2 tasks`));
      button("Undo", undoStrip()).click();
      await wait(() => ["task-31","task-32"].every(id => !of(id).labels.includes(bulkLabel)));
      check("Undo puts back each task's own labels with a revision-checked write",
        before.every(entry => JSON.stringify(of(entry.id).labels) === JSON.stringify(entry.labels) && of(entry.id).revision === entry.revision + 2));
      check("an undo is not offered for undo again", !undoStrip());
      root.querySelector('[data-testid="task-selection-change"]')?.click(); await settle();
      button("Move to…", menu()).click(); await settle(); button("Backlog", menu()).click();
      await wait(() => ["task-31","task-32"].every(id => of(id).status === "backlog"));
      check("a bulk move from the selection bar moves every selected task", Boolean(card("task-31")?.closest('[data-task-column="backlog"]')));
      card("task-31").dispatchEvent(new KeyboardEvent("keydown",{key:"z",metaKey:true,ctrlKey:true,bubbles:true,cancelable:true}));
      await wait(() => ["task-31","task-32"].every(id => of(id).status === "ready"));
      check("Command- or Control-Z undoes the last board change from the keyboard", Boolean(card("task-31")?.closest('[data-task-column="ready"]')) && !undoStrip());
      // Archive sets the flag; its undo clears it, and the status never moved.
      root.querySelector('[data-testid="task-archive-selected"]').click(); await settle();
      await wait(() => ["task-31","task-32"].every(id => of(id).archived === true));
      check("bulk Archive offers its own undo", undoStrip()?.textContent.includes("Archived 2 tasks"));
      button("Undo", undoStrip()).click();
      await wait(() => ["task-31","task-32"].every(id => of(id).archived === false));
      await wait(() => card("task-32"));
      check("undoing an archive returns each task to the column it left", Boolean(card("task-32")?.closest('[data-task-column="ready"]')) && of("task-32").status === "ready");
      // An undo never reverts work someone did after the change.
      if (root.querySelector(".selection")) await click("Clear task selection");
      await pick("task-31"); await pick("task-32");
      root.querySelector('[data-testid="task-archive-selected"]').click(); await wait(() => of("task-32").archived === true && of("task-31").archived === true);
      const edited = of("task-32"); tasks = [...tasks.filter(task => task.id !== "task-32"), {...edited, title: "Edited elsewhere", revision: edited.revision + 1}];
      button("Undo", undoStrip()).click(); await wait(() => of("task-31").archived === false); await settle(100);
      check("Undo is refused for a task edited since, says so, and still undoes the rest",
        of("task-32").archived === true && of("task-32").title === "Edited elsewhere" && of("task-31").archived === false && root.textContent.includes("changed while you were moving it"));
      if (root.querySelector(".selection")) await click("Clear task selection");
    } catch (error) { check(`the bulk edits and undo checks ran to the end (${error.message})`, false); }

    // ---- Filing a task as a GitHub issue ----------------------------------
    {
      const issuePrompt = () => [...document.querySelectorAll('[role="dialog"]')].filter(node => node.getAttribute("aria-label") === "Create GitHub issue").at(-1);
      // By its stem: a selection's row reads "Create 2 GitHub issues…".
      const issueRow = () => menu() && [...menu().querySelectorAll("button")].find(el => /^Create (\d+ )?GitHub issues?…/.test(el.textContent.trim()));
      const labelsOf = id => tasks.find(task => task.id === id).labels;
      const writesBefore = writes.length;
      await openMenu("task-20");
      check("a card's menu offers Create GitHub issue", Boolean(issueRow()) && !issueRow().disabled);
      issueRow().click(); await wait(() => Boolean(issuePrompt()));
      const promptText = issuePrompt().textContent;
      check("the confirmation names the task, the repository and the checkout its remote is read from",
        promptText.includes("Ready task 11") && promptText.includes("GitHub remote of GitPulse")
        && promptText.includes("/fixture/GitPulse") && promptText.includes("inferred"));
      check("nothing is published before the reader confirms", issueCalls.length === 0);
      button("Create issue", issuePrompt()).click();
      await wait(() => labelsOf("task-20").includes("issue-77"));
      await settle(100);
      const sent = issueCalls[0];
      check(`the issue is filed through the guarded command on the task's own checkout (sent ${JSON.stringify(sent && { repoPath: sent.repoPath, title: sent.title, labels: sent.labels })})`,
        issueCalls.length === 1 && sent.repoPath === "/fixture/GitPulse" && sent.title === "Ready task 11"
        && Array.isArray(sent.labels) && sent.labels.length === 0);
      check("the issue body carries the description and no local path",
        sent.body.includes("Keep changes focused and verify the result.") && !sent.body.includes("/fixture"));
      check("the task is linked to the new issue in one revision-checked write",
        writes.length === writesBefore + 1 && writes.at(-1).id === "task-20" && writes.at(-1).expected_revision === 1);
      await openMenu("task-20");
      check("a linked task names its issue instead of offering a duplicate",
        Boolean(issueRow()) && issueRow().disabled && issueRow().textContent.includes("Linked to #77"));
      menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();

      await openMenu("task-21"); issueRow().click(); await wait(() => Boolean(issuePrompt()));
      button("Cancel", issuePrompt()).click(); await settle(150);
      check("cancelling the confirmation files nothing and writes nothing",
        issueCalls.length === 1 && writes.length === writesBefore + 1 && !labelsOf("task-21").some(label => label.startsWith("issue-")));

      issueFailure = "gh: To get started with GitHub CLI, please run: gh auth login";
      await openMenu("task-22"); issueRow().click(); await wait(() => Boolean(issuePrompt()));
      button("Create issue", issuePrompt()).click(); await settle(200);
      const alertText = [...root.querySelectorAll('[role="alert"]')].map(node => node.textContent).join(" ");
      check(`a refused creation says so and links nothing (${alertText.slice(0, 160)})`,
        alertText.includes("Not created — “Ready task 13”") && alertText.includes("gh auth login")
        && writes.length === writesBefore + 1 && !labelsOf("task-22").some(label => label.startsWith("issue-")));
      issueFailure = "";

      // ---- Several tasks in one run ----
      const boardAlert = () => [...root.querySelectorAll('[role="alert"]')].map(node => node.textContent).join(" ");
      const multiSelect = async ids => {
        if (root.querySelector('[aria-label="Selected task actions"]')) await click("Clear task selection");
        for (const id of ids) { card(id).dispatchEvent(new MouseEvent("click",{bubbles:true,ctrlKey:true})); await settle(); }
      };
      const batchPrompt = () => [...document.querySelectorAll('[role="dialog"]')].filter(node => node.getAttribute("aria-label") === "Create GitHub issues").at(-1);
      await multiSelect(["task-23", "task-24", "task-20"]);
      await openMenu("task-23");
      check(`a selection offers one run and counts the linked task it will skip (${issueRow()?.textContent.trim()})`,
        Boolean(issueRow()) && !issueRow().disabled && issueRow().textContent.includes("Create 2 GitHub issues") && issueRow().textContent.includes("1 linked"));
      const callsBeforeBatch = issueCalls.length;
      issueRow().click(); await wait(() => Boolean(batchPrompt()));
      const batchText = batchPrompt().textContent;
      check("the run asks once, listing every issue and the task it will not file",
        batchText.includes("Create 2 issues on GitHub?") && batchText.includes("Ready task 14") && batchText.includes("Ready task 15")
        && batchText.includes("Not filed (1)") && batchText.includes("Ready task 11 — already linked to #77")
        && issueCalls.length === callsBeforeBatch);
      button("Create 2 issues", batchPrompt()).click();
      await wait(() => labelsOf("task-24").some(label => label.startsWith("issue-")));
      await settle(100);
      const batchCalls = issueCalls.slice(callsBeforeBatch);
      check(`the run files each task in board order and links each to its own issue (${batchCalls.map(call => call.title).join(", ")})`,
        batchCalls.length === 2 && batchCalls[0].title === "Ready task 14" && batchCalls[1].title === "Ready task 15"
        && labelsOf("task-23").includes(`issue-${77 + callsBeforeBatch}`) && labelsOf("task-24").includes(`issue-${78 + callsBeforeBatch}`)
        && labelsOf("task-20").filter(label => label.startsWith("issue-")).length === 1);

      // A refusal partway stops the run: nothing after it is filed.
      await multiSelect(["task-25", "task-26", "task-27"]);
      await openMenu("task-25");
      const callsBeforeStop = issueCalls.length;
      issueFailure = "gh: HTTP 403: Resource not accessible"; issueFailOnCall = callsBeforeStop + 2;
      issueRow().click(); await wait(() => Boolean(batchPrompt()));
      button("Create 3 issues", batchPrompt()).click(); await settle(300);
      check(`a refusal partway stops the run and says what was and was not filed (${boardAlert().slice(0, 200)})`,
        issueCalls.length === callsBeforeStop + 2
        && labelsOf("task-25").some(label => label.startsWith("issue-"))
        && !labelsOf("task-26").some(label => label.startsWith("issue-")) && !labelsOf("task-27").some(label => label.startsWith("issue-"))
        && boardAlert().includes("Not created — “Ready task 17”") && boardAlert().includes("Stopped there; 1 more task was not filed."));
      issueFailure = ""; issueFailOnCall = 0;

      // Re-running the same selection resumes: the filed task is skipped.
      await multiSelect(["task-25", "task-26", "task-27"]);
      await openMenu("task-26");
      check("re-running a stopped selection skips what it already filed",
        Boolean(issueRow()) && issueRow().textContent.includes("Create 2 GitHub issues") && issueRow().textContent.includes("1 linked"));

      // Stop takes effect before the next creation, never mid-gh.
      holdIssue = true;
      const callsBeforeHold = issueCalls.length;
      issueRow().click(); await wait(() => Boolean(batchPrompt()));
      button("Create 2 issues", batchPrompt()).click();
      await wait(() => Boolean(root.querySelector('[data-testid="task-issue-progress"]')) && issueCalls.length === callsBeforeHold + 1);
      const progress = root.querySelector('[data-testid="task-issue-progress"]');
      const progressText = progress.textContent;
      button("Stop", progress).click(); await settle();
      holdIssue = false; releaseIssue?.(); await settle(300);
      check(`Stop finishes the creation in flight and files nothing more (${progressText.trim()}; ${boardAlert().slice(0, 160)})`,
        progressText.includes("Filing issue 1 of 2") && issueCalls.length === callsBeforeHold + 1
        && labelsOf("task-26").some(label => label.startsWith("issue-")) && !labelsOf("task-27").some(label => label.startsWith("issue-"))
        && boardAlert().includes("Stopped as asked; 1 task was not filed."));

      // gh can publish and then miss its deadline: that outcome is unknown,
      // and calling it "not created" would invite a duplicate.
      issueFailure = "gh timed out after 90s";
      await multiSelect(["task-27"]);
      await openMenu("task-27"); issueRow().click(); await wait(() => Boolean(issuePrompt()));
      button("Create issue", issuePrompt()).click(); await settle(300);
      check(`a creation that hit its deadline is reported as unknown, with the title to look for (${boardAlert().slice(0, 200)})`,
        boardAlert().includes("Could not confirm whether “Ready task 18” was filed")
        && boardAlert().includes("Check GitHub for an issue titled “Ready task 18”") && !boardAlert().includes("Not created"));
      issueFailure = "";

      // An issue created but not linked keeps a way to link it, even after
      // the board reloads, and linking it files nothing new.
      failIssueLinkOnce = true;
      await multiSelect(["task-28"]);
      await openMenu("task-28"); issueRow().click(); await wait(() => Boolean(issuePrompt()));
      const callsBeforeRelink = issueCalls.length;
      button("Create issue", issuePrompt()).click(); await settle(300);
      const relinkBanner = () => root.querySelector('[data-testid="task-issue-relink"]');
      const createdNumber = 77 + callsBeforeRelink;
      check(`a created issue the task could not be linked to stays linkable (${relinkBanner()?.textContent.trim()})`,
        Boolean(relinkBanner()) && relinkBanner().textContent.includes(`#${createdNumber}`)
        && boardAlert().includes(`Created issue #${createdNumber} for “Ready task 19”, but the task is not linked`)
        && !labelsOf("task-28").some(label => label.startsWith("issue-")));
      await click("Refresh tasks"); await settle(200);
      check("a board reload does not drop the way to link it", Boolean(relinkBanner()));
      // Re-running before linking must not file the same task a second time.
      await multiSelect(["task-28"]);
      await openMenu("task-28"); issueRow().click(); await settle(300);
      check(`re-running a task whose issue exists but is unlinked files nothing (${boardAlert().slice(0, 200)})`,
        issueCalls.length === callsBeforeRelink + 1 && !issuePrompt()
        && boardAlert().includes(`issue #${createdNumber} already exists for it; link it instead`));
      button(`Link to #${createdNumber}`, relinkBanner()).click();
      await wait(() => labelsOf("task-28").includes(`issue-${createdNumber}`)); await settle(100);
      check("linking it afterwards writes the label, files nothing new, and clears the offer",
        !relinkBanner() && issueCalls.length === callsBeforeRelink + 1);
    }

    // ---- Handing a card to an agent --------------------------------------
    await openMenu("task-11"); button("Send to agent…", menu()).click(); await settle();
    const agentRows = [...menu().querySelectorAll("button")].map(el => el.textContent.trim());
    check("the agent submenu offers Grok and Antigravity beside Codex and Claude Code",
      agentRows.some(text => text.startsWith("Grok")) && agentRows.some(text => text.startsWith("Antigravity"))
      && agentRows.some(text => text.startsWith("Claude")) && agentRows.some(text => text.startsWith("Codex")));
    const target = [...menu().querySelectorAll("button")].find(el => el.textContent.trim().startsWith("Codex"));
    target.click(); await settle(250);
    const sheet = () => document.querySelector('[data-testid="task-handoff"]');
    check("the agent handoff opens from a card in two clicks, and launches nothing on its own",
      Boolean(sheet()) && preparedRuns.length === 0);
    check("the handoff sheet shows what it would run before it runs it",
      sheet().textContent.includes("Saved revision") && Boolean(sheet().querySelector('[data-testid="task-handoff-form"]')));
    // The ready hint is written the way the sheet writes it. Hard-coding the
    // macOS glyphs made this assert the host as well as the gate: off macOS
    // `shortcutTextLabel` spells ⌘ as "Ctrl+", so the comparison was false for
    // a ready gate and the check inverted on the Linux runner while passing on
    // every Mac it was written on.
    const launchHint = `${shortcutTextLabel("⌘↩", get(hostPlatform).os)} to launch`;
    check("the handoff names the one thing left to do rather than a dead button",
      button("Launch in Codex", sheet()).disabled === (sheet().querySelector(".gate").textContent.trim() !== launchHint));
    sheet().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
    check("Escape closes the handoff without preparing a run", !sheet() && preparedRuns.length === 0);
    // A launch that succeeds closes the sheet from inside the form's launch:
    // `onLaunched` nulls the board's handoff while `launch()` is still on the
    // stack, and the component is not destroyed until the next flush. A read
    // of a prop after that callback dereferenced the null handoff and crashed
    // the Tasks pane after every successful board launch.
    acceptPreparation = true;
    const crashesBeforeLaunch = crashes.length;
    await openMenu("task-11"); button("Send to agent…", menu()).click(); await settle();
    [...menu().querySelectorAll("button")].find(el => el.textContent.trim().startsWith("Codex")).click(); await settle(250);
    [...sheet().querySelectorAll('[role="group"][aria-label="Connection"] button')].find(el => el.textContent.trim() === "Managed")?.click(); await settle();
    const startManaged = button("Start managed Codex", sheet());
    check(`the sheet offers a managed start once the gate is open (${sheet()?.querySelector(".gate")?.textContent.trim()})`, Boolean(startManaged) && !startManaged.disabled);
    startManaged.click();
    try { await wait(() => !sheet() || crashes.length > crashesBeforeLaunch); }
    catch { throw Error(`the launch neither closed the sheet nor crashed: ${sheet()?.textContent.trim().slice(-500)} / prepared=${preparedRuns.length}`); }
    await settle(100);
    check(`a successful launch closes the sheet without crashing the board (${crashes.slice(crashesBeforeLaunch).join(" | ")})`,
      !sheet() && crashes.length === crashesBeforeLaunch && preparedRuns.length === 1 && fixtureRuns.size === 1
      && [...fixtureRuns.values()][0].state === "running");
    // Again, pressed twice in one tick — the button and ⌘↩
    // reach the same `launch()`. One attempt, one close, and still no crash.
    await openMenu("task-11"); button("Send to agent…", menu()).click(); await settle();
    [...menu().querySelectorAll("button")].find(el => el.textContent.trim().startsWith("Codex")).click(); await settle(250);
    [...sheet().querySelectorAll('[role="group"][aria-label="Connection"] button')].find(el => el.textContent.trim() === "Managed")?.click(); await settle();
    button("Start managed Codex", sheet()).click();
    sheet().dispatchEvent(new KeyboardEvent("keydown", {key: "Enter", metaKey: true, ctrlKey: true, bubbles: true, cancelable: true}));
    try { await wait(() => !sheet() || crashes.length > crashesBeforeLaunch); }
    catch { throw Error(`the second launch neither closed the sheet nor crashed: ${sheet()?.textContent.trim().slice(-500)}`); }
    await settle(100);
    check(`a second launch, pressed twice at once, prepares one attempt and closes cleanly (${crashes.slice(crashesBeforeLaunch).join(" | ")})`,
      !sheet() && crashes.length === crashesBeforeLaunch && preparedRuns.length === 2 && fixtureRuns.size === 2
      && preparedRuns[1].task_id === "task-11" && preparedRuns[1].id !== preparedRuns[0].id);
    acceptPreparation = false;

    // ---- A model for one launch -------------------------------------------
    // The override rides on the preparation as `model_choice`; the host
    // overlays it on the saved default and records the result on the run.
    prepareRefusal = "invalid_input";
    await openMenu("task-11"); button("Send to agent…", menu()).click(); await settle();
    [...menu().querySelectorAll("button")].find(el => el.textContent.trim().startsWith("Claude")).click(); await settle(250);
    [...sheet().querySelectorAll('[role="group"][aria-label="Connection"] button')].find(el => el.textContent.trim() === "Terminal")?.click(); await settle();
    const override = () => sheet()?.querySelector('[data-testid="handoff-model-override"]');
    const overrideField = name => override()?.querySelector(`[aria-label="${name}"]`);
    check("a terminal handoff offers this launch's model, effort and advisor for Claude Code",
      Boolean(overrideField("Model")) && Boolean(overrideField("Effort")) && Boolean(overrideField("Advisor model")));
    await change(overrideField("Model"), "opus 4");
    check("a model that is not a model name closes the gate, by name, instead of being dropped",
      sheet().querySelector(".gate").textContent.includes("“opus 4” is not a model name"));
    await change(overrideField("Model"), "opus");
    await change(overrideField("Effort"), "high", "change");
    const preparedBefore = preparedRuns.length;
    const launchClaude = [...sheet().querySelectorAll("button")].find(el => el.textContent.trim().startsWith("Launch in"));
    launchClaude?.click(); await wait(() => preparedRuns.length > preparedBefore); await settle(100);
    check("the launch sends only the fields typed, as this attempt's model_choice",
      JSON.stringify(preparedRuns.at(-1).model_choice) === JSON.stringify({ model: "opus", effort: "high" }) && preparedRuns.at(-1).provider === "claude");
    [...sheet().querySelectorAll('[role="group"][aria-label="Connection"] button')].find(el => el.textContent.trim() === "Managed")?.click(); await settle();
    check("a managed attempt offers no model override, and the typed one is not carried into it",
      !override() && !sheet().querySelector(".gate").textContent.includes("model"));
    sheet().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
    prepareRefusal = "store_error";

    // ---- Checklist and linked tasks in the sheet --------------------------
    try {
      card("task-11").click(); await wait(editor);
      const relations = () => editor().querySelector('[data-testid="task-relations"]');
      check("the task sheet shows a checklist and linked tasks", Boolean(relations()));
      const entry = relations().querySelector('[data-testid="task-checklist-add"]');
      await change(entry, "Write the regression test");
      entry.dispatchEvent(new KeyboardEvent("keydown", {key: "Enter", bubbles: true, cancelable: true})); await settle();
      relations().querySelector('[data-testid="task-checklist"] input[type="checkbox"]').click(); await settle();
      check("a checklist item is added and ticked in place, and the sheet is unsaved",
        relations().querySelector('[data-testid="task-checklist-count"]')?.textContent.trim() === "1 of 1 done"
        && editor().querySelector("header small")?.textContent === "Unsaved");
      await change(relations().querySelector('[aria-label="Link kind"]'), "blocks", "change");
      await change(relations().querySelector('[data-testid="task-link-search"]'), "Ready task 05");
      button("Find", relations()).click();
      await wait(() => relations().querySelector('[data-testid="task-link-results"] button'));
      const picked = relations().querySelector('[data-testid="task-link-results"] button');
      const pickedTitle = picked.textContent.trim();
      picked.click(); await settle();
      check("a found task is linked by kind, and named by its title",
        relations().querySelector('[data-testid="task-links"]')?.textContent.includes("Blocks")
        && relations().querySelector('[data-testid="task-links"]')?.textContent.includes(pickedTitle));
      const linkedId = tasks.find(task => task.title === pickedTitle)?.id;
      await click("Save task"); await wait(() => of("task-11").checklist?.length === 1);
      check("saving writes the checklist and links as stored fields",
        JSON.stringify(of("task-11").checklist) === JSON.stringify([{ text: "Write the regression test", done: true }])
        && JSON.stringify(of("task-11").links) === JSON.stringify([{ kind: "blocks", item_id: linkedId }]));
      confirmAnswer = true; await click("Close task details"); await settle(100);
    } catch (error) { check(`the checklist and link checks ran to the end (${error.message})`, false); if (editor()) { confirmAnswer = true; await click("Close task details"); } }

    // ---- Merge from the board ---------------------------------------------
    try {
      const mergeButton = () => root.querySelector('[data-testid="task-merge-selected"]');
      const mergeDialog = () => document.querySelector('[data-testid="task-merge-dialog"]');
      const pickCard = async id => { card(id).dispatchEvent(new MouseEvent("click",{bubbles:true,ctrlKey:true})); await settle(); };
      const readyIds = [...root.querySelectorAll('[data-task-column="ready"] [data-task-card]')].map(el => el.getAttribute("data-card-id"))
        .filter(id => !["task-11", "task-31", "task-32"].includes(id));
      const [keep, fold, partial, shared] = readyIds;
      if (!keep || !fold || !partial || !shared) throw Error("Merge checks need four Ready cards");
      if (root.querySelector(".selection")) await click("Clear task selection");
      // A task shared with another repository is never offered: deleting it
      // would delete it everywhere, and the merge refuses it.
      const sharedTask = tasks.find(task => task.id === shared);
      sharedTask.repository_ids = ["repo-0", "repo-1"]; sharedTask.revision += 1;
      await click("Refresh tasks"); await wait(() => card(shared));
      await pickCard(shared); await pickCard(keep);
      check("Merge is not offered for a selection holding a shared task, and says why",
        Boolean(mergeButton()) && mergeButton().disabled && mergeButton().title.includes("several repositories"));
      await click("Clear task selection");
      await pickCard(keep); await pickCard(fold); await pickCard(partial);
      check("Merge is offered for cards of one repository", Boolean(mergeButton()) && !mergeButton().disabled);
      mergeButton().click(); await wait(mergeDialog);
      const run = () => mergeDialog().querySelector('[data-testid="task-merge-run"]');
      check("the merge waits for a reason before it writes anything", run().disabled && mergeWrites.length === 0);
      await change(mergeDialog().querySelector('[data-testid="task-merge-reason"]'), "Same notification bug");
      mergeFailSource = partial;
      const revisions = Object.fromEntries([keep, fold, partial].map(id => [id, tasks.find(task => task.id === id).revision]));
      run().click(); await wait(() => mergeDialog()?.querySelector('[data-testid="task-merge-outcome"]'));
      const sent = mergeWrites.at(-1);
      check("the board merge sends the card it keeps and each source at the revision the board drew",
        sent?.repository_id === "repo-0" && sent.into.id === keep && sent.into.expected_revision === revisions[keep]
        && JSON.stringify(sent.sources) === JSON.stringify([fold, partial].map(id => ({ id, expected_revision: revisions[id] }))) && sent.reason === "Same notification bug");
      check("a delete that fails after its reason is recorded reads as a partial merge, naming that card",
        mergeDialog().querySelector('[data-testid="task-merge-outcome"]').textContent.includes("Merged part of the selection")
        && mergeDialog().textContent.includes("Still on the board") && mergeDialog().textContent.includes("deleting it failed")
        && Boolean(button("Review and run again", mergeDialog())));
      button("Done", mergeDialog()).click(); await settle(300);
      check("the merged source leaves the board and the one that failed stays",
        !mergeDialog() && !card(fold) && Boolean(card(partial)) && Boolean(card(keep)));
      mergeFailSource = "";
      if (root.querySelector(".selection")) await click("Clear task selection");
    } catch (error) { check(`the merge checks ran to the end (${error.message})`, false); if (document.querySelector('[data-testid="task-merge-dialog"]')) button("Cancel", document.querySelector('[data-testid="task-merge-dialog"]'))?.click(); }


    // ---- Which model writes the text ------------------------------------
    const enginePicks = () => root.querySelector('[data-testid="task-assist-engine"]');
    const appleButton = () => [...(enginePicks()?.querySelectorAll("button") ?? [])].find(el => el.textContent.includes("Apple Intelligence"));
    await click("New task");
    await change(editor().querySelector(".notes-label textarea"), "notifications go to the wrong task after a rename");
    check("a Mac with the on-device model offers it beside Manvi",
      Boolean(enginePicks()) && Boolean(appleButton()) && !appleButton().disabled
      && appleButton().textContent.includes("On this Mac"));
    appleButton().click(); await settle();
    check("choosing the on-device engine renames the action and says where it runs",
      /Apple Intelligence/.test(enhanceButton().textContent) && editor().textContent.includes("Nothing leaves this Mac"));
    const writesBefore = enhancementWrites.length;
    enhanceButton().click(); await wait(() => button("Accept selected fields"));
    const appleWrites = enhancementWrites.slice(writesBefore);
    check("the on-device draft is one proposal in the same store, generated here",
      appleWrites[0].method === "enhancements.create" && appleWrites[0].provider === "apple-intelligence"
      && appleWrites.some(write => write.method === "enhancements.complete")
      && !appleWrites.some(write => write.method === "enhancements.generate"));
    // `prepareTask` saves the draft first, which promotes raw notes into the
    // title and description — so by now the text is in those fields, not in
    // `notes`, and the ask is an "improve" rather than a "draft".
    check("the task text reaches the on-device model, named, and nothing else does",
      appleDrafts.length === 1
      && [appleDrafts[0].notes, appleDrafts[0].title, appleDrafts[0].description].join(" ").includes("wrong task after a rename")
      && appleDrafts[0].context.includes(`Repository: ${repos[0].name}`)
      && !("model" in appleDrafts[0]) && !("base_url" in appleDrafts[0]));
    // "Exactly like a Manvi one" is the point: one review, both fields
    // selectable, one accept, one dismiss — whoever wrote the text.
    check("an on-device suggestion is reviewed exactly like a Manvi one",
      (() => {
        const article = editor().querySelector('article[aria-label="Enhancement review"]');
        if (!article) return false;
        const labels = [...article.querySelectorAll("label")].map(el => el.textContent.trim());
        return labels.includes("Title") && labels.includes("Description")
          && Boolean(button("Accept selected fields", article)) && Boolean(button("Dismiss", article));
      })());
    await click("Accept selected fields"); await settle(150);
    const drafted = [...proposals.values()].at(-1);
    check("accepting an on-device suggestion goes through the same acceptance",
      drafted.state === "accepted" && field("Title").value === "Route notifications to the right task");
    confirmAnswer = true; await click("Close task details"); await settle();

    // A Mac that could run it but has it switched off must say so and refuse,
    // not present a button that fails when pressed.
    appleStatus = { compiled: true, state: "unavailable", reason: "apple_intelligence_not_enabled", detail: "Turn on Apple Intelligence in System Settings to use it here." };
    await click("New task"); await settle();
    check("an engine that is switched off is offered but not selectable",
      Boolean(appleButton()) && appleButton().disabled && appleButton().textContent.includes("Turned off"));
    check("a Mac with it switched off still drafts with Manvi",
      /Manvi/.test(enhanceButton().textContent));
    confirmAnswer = true; await click("Close task details"); await settle();

    // A build with no bridge hides the choice entirely: a permanently disabled
    // option is a worse answer than no option.
    appleStatus = { compiled: false, state: "unsupported_os", reason: "not_compiled", detail: "This build has no Apple Intelligence support." };
    await click("New task"); await settle();
    check("a build with no bridge offers no engine picker at all", !enginePicks());
    confirmAnswer = true; await click("Close task details"); await settle();

    // The back link from a terminal session (`taskOpen.ts`): a task asked
    // for from outside the board opens on the global board, exactly once,
    // and a repository's own board leaves the request for the global one.
    {
      const titleOf = id => tasks.find(task => task.id === id)?.title;
      if (editor()) { confirmAnswer = true; await click("Close task details"); await settle(); }
      await click("GitPulse fixture"); await settle(400);
      requestTaskOpen("task-3");
      await settle(400);
      check("a repository's board leaves a task request for the global board",
        get(taskOpenRequest) === "task-3" && !editor());
      await click("Global fixture"); await settle(400);
      await wait(() => editor() && field("Title")?.value === titleOf("task-3"));
      check("the global board opens the task a session links to, and takes the request",
        field("Title")?.value === titleOf("task-3") && get(taskOpenRequest) === null);
      requestTaskOpen("task-1");
      await wait(() => field("Title")?.value === titleOf("task-1"));
      check("a second link opens its task over the first", get(taskOpenRequest) === null);
      requestTaskOpen("task-3");
      await wait(() => field("Title")?.value === titleOf("task-3"));
      check("linking back to a task already opened once opens it again", get(taskOpenRequest) === null);
      // task-1's tab is still open, so the sheet shows it; a dead link must
      // say so and leave that task where it was.
      const before = field("Title")?.value;
      requestTaskOpen("no-such-task");
      await settle(400);
      check("a link to a task that no longer exists says so rather than doing nothing",
        get(taskOpenRequest) === null && field("Title")?.value === before
        && /\S/.test(root.querySelector('[role="alert"]')?.textContent ?? ""));
      confirmAnswer = true; await click("Close task details"); await settle();
    }

    // The board marks each card with the agents working on its task, from
    // the same reads and judgement as the task's Agents pane: how many, and
    // whether one needs the reader. Nothing was read, nothing is marked.
    {
      const refreshBoard = async () => { root.querySelector('button[aria-label="Refresh"]').click(); await settle(200); };
      const chip = id => root.querySelector(`[data-card-id="${id}"] [data-testid="card-agents"]`);
      check("a card with no agent working carries no agent mark", !root.querySelector('[data-testid="card-agents"]'));
      const now = Math.floor(Date.now() / 1000);
      const base = {revision:1, updated_at:now, task_id:"task-1", source_revision:1, task_title:"Fixture", repository_id:repos[0].id, provider:"codex", permission_mode:"ask", cwd:"/fixture/GitPulse", created_at:now, expires_at:now + 600, exit_code:null, reason:"", outcome_uncertain:false};
      boardRuns = [
        {...base, id:"board-terminal", kind:"external_terminal", state:"running", session_id:"board-term"},
        {...base, id:"board-managed", kind:"managed", state:"running", session_id:"board-managed-session", provider_state:"running", provider_thread_id:"thread", provider_turn_id:"turn"},
        {...base, id:"board-ended", task_id:"task-3", kind:"external_terminal", state:"exited", session_id:null, exit_code:0},
      ];
      await refreshBoard();
      await wait(() => chip("task-1"));
      check("a card counts the agents working on its task, and only those",
        chip("task-1").textContent.startsWith("2") && !chip("task-1").dataset.asking && !chip("task-3"));
      const payload = '{"command":"cargo test"}';
      const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(payload)))].map(b => b.toString(16).padStart(2, "0")).join("");
      boardDecisions = [{id:"board-request", revision:1, updated_at:now, run_id:"board-managed", task_id:"task-1", source_revision:1, repository_id:repos[0].id, repository_revision:repos[0].revision, owner_id:"fixture", session_id:"board-managed-session", provider_thread_id:"thread", provider_turn_id:"turn", protocol_request_id:"s:board", provider:"codex", permission_mode:"ask", policy_revision:1, cwd:"/fixture/GitPulse", kind:"permission", payload, payload_digest:digest, created_at:now, expires_at:now + 300, state:"pending", decision:null, answer:null, actionable:true, reason:""}];
      await refreshBoard();
      await wait(() => chip("task-1")?.dataset.asking === "1");
      check("a card whose agent is waiting on a request says it needs you",
        chip("task-1").textContent.includes("1 needs you") && chip("task-1").dataset.tone === "needs-you");
      boardRunsFail = true;
      await refreshBoard();
      await wait(() => root.querySelector('[data-testid="board-agents-error"]'));
      check("a failed read clears the marks and says so, rather than keep old ones", !root.querySelector('[data-testid="card-agents"]'));
      boardRunsFail = false; boardRuns = []; boardDecisions = [];
      await refreshBoard();
      await wait(() => !root.querySelector('[data-testid="board-agents-error"]'));
      check("once every agent has ended, no card is marked", !root.querySelector('[data-testid="card-agents"]'));
    }

    // ---- Keyboard: moving between cards, selecting and reordering ----------
    try {
      const column = status => [...root.querySelectorAll(`[data-task-column="${status}"] [data-task-card]`)];
      const press = (el, key, extra = {}) => el.dispatchEvent(new KeyboardEvent("keydown",{key,bubbles:true,cancelable:true,...extra}));
      const ready = column("ready");
      ready[0].focus(); press(ready[0], "ArrowDown"); await settle();
      check("ArrowDown moves focus to the next card in the column", document.activeElement === column("ready")[1]);
      press(document.activeElement, "End"); await settle();
      check("End moves focus to the column's last card", document.activeElement === column("ready").at(-1));
      press(document.activeElement, "Home"); await settle();
      check("Home moves focus to the column's first card", document.activeElement === column("ready")[0]);
      press(document.activeElement, "ArrowUp"); await settle();
      check("ArrowUp at the top stays put rather than leaving the column", document.activeElement === column("ready")[0]);
      const firstId = document.activeElement.dataset.cardId;
      press(document.activeElement, "x"); await settle();
      check("x selects the focused card without a modifier", root.querySelector('[aria-label="Selected task actions"]')?.textContent.includes("1 selected"));
      check("a selected card says so to a screen reader", card(firstId)?.querySelector(".sr-only")?.textContent.includes("selected"));
      press(document.activeElement, "ArrowDown", {shiftKey:true}); await settle();
      press(document.activeElement, "ArrowDown", {shiftKey:true}); await settle();
      check("Shift+ArrowDown extends the selection with the focus", root.querySelector('[aria-label="Selected task actions"]')?.textContent.includes("3 selected") && document.activeElement === column("ready")[2]);
      document.activeElement.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
      check("Escape clears a keyboard selection", !root.querySelector('[aria-label="Selected task actions"]'));
      const target = column("ready")[0]; const targetId = target.dataset.cardId;
      target.focus(); const writesBeforeNudge = writes.length;
      press(target, "ArrowDown", {altKey:true});
      await wait(() => column("ready")[1]?.dataset.cardId === targetId);
      check("Alt+ArrowDown moves a card down its column with one write", writes.length > writesBeforeNudge && column("ready")[1]?.dataset.cardId === targetId);
      check("the moved card keeps the focus", document.activeElement?.dataset?.cardId === targetId);
      press(document.activeElement, "ArrowUp", {altKey:true});
      await wait(() => column("ready")[0]?.dataset.cardId === targetId);
      check("Alt+ArrowUp moves it back", column("ready")[0]?.dataset.cardId === targetId);
      check("every card names the keys it answers to",
        ["ArrowUp","ArrowDown","Home","End","Shift+ArrowDown","Alt+ArrowUp","X","ContextMenu"].every(key => target.getAttribute("aria-keyshortcuts")?.split(" ").includes(key)));
      await click("List view");
      const listRows = () => [...root.querySelectorAll('[data-task-list] [data-task-card]')];
      check("the list layout is a labelled group", root.querySelector('[aria-label="Task list"]')?.getAttribute("role") === "group");
      listRows()[0].focus(); press(listRows()[0], "ArrowDown"); await settle();
      check("ArrowDown moves through the list layout too", document.activeElement === listRows()[1]);
      press(document.activeElement, "End"); await settle();
      check("End reaches the last row of the list", document.activeElement === listRows().at(-1));
      await click("Board view");
      if (root.querySelector('[data-testid="task-undo"]')) { button("Dismiss undo", root.querySelector('[data-testid="task-undo"]'))?.click(); await settle(); }
    } catch (error) { check(`the keyboard operation checks ran to the end (${error.message})`, false); }

    // ---- Task fields: the home workspace can be changed ---------------------
    try {
      if (editor()) { confirmAnswer = true; await click("Close task details"); }
      card("task-3").click(); await wait(editor);
      const home = () => field("Home workspace");
      check("the task sheet offers the home workspace as a field", home() instanceof HTMLSelectElement && [...home().options].some(option => option.value === "workspace"));
      await change(home(), "workspace", "change");
      await click("Save task");
      await wait(() => tasks.find(task => task.id === "task-3")?.home_workspace_id === "workspace");
      check("saving writes the chosen home workspace and keeps the rest of the task",
        writes.at(-1).id === "task-3" && writes.at(-1).home_workspace_id === "workspace" && writes.at(-1).description === "Keep changes focused and verify the result.");
      confirmAnswer = true; await click("Close task details");
      card("task-3").click(); await wait(editor);
      check("the home workspace reads back when the task is opened again", home()?.value === "workspace");
      const none = [...home().options].find(option => option.textContent.trim() === "None");
      await change(home(), none?.value ?? "", "change");
      await click("Save task");
      await wait(() => (tasks.find(task => task.id === "task-3")?.home_workspace_id ?? null) === null);
      check("choosing None saves the task with no home workspace", writes.at(-1).id === "task-3" && (writes.at(-1).home_workspace_id ?? null) === null);
      confirmAnswer = true; await click("Close task details"); await settle(100);
    } catch (error) { check(`the home workspace checks ran to the end (${error.message})`, false); }

    // ---- Workspaces: reorder, persisted through a reload ------------------
    try {
      const navRows = () => [...root.querySelectorAll('nav[aria-label="Task scopes"] [data-workspace-row]')].map(row => row.dataset.workspaceRow);
      const reload = async () => { await click("GitPulse fixture"); await settle(300); await click("Global fixture"); await wait(() => navRows().length > 0); await settle(200); };
      check("the first workspace cannot move further up", button("Move Developer tools up")?.disabled === true);
      const before = workspaceWrites.length;
      const spaceBefore = structuredClone(workspaces.find(space => space.id === "workspace-empty"));
      button("Move Fresh space up").click();
      await wait(() => navRows()[0] === "workspace-empty");
      check("a workspace moves up the navigator with one stored position write",
        workspaceWrites.length === before + 1 && workspaceWrites.at(-1).id === "workspace-empty" && workspaceWrites.at(-1).position < workspace.position);
      const movedSpace = workspaces.find(space => space.id === "workspace-empty");
      check("the move changes the position and nothing else about the workspace",
        ["name", "description", "icon", "color", "pinned", "archived"].every(key => movedSpace[key] === spaceBefore[key])
        && JSON.stringify(movedSpace.repository_ids) === JSON.stringify(spaceBefore.repository_ids) && movedSpace.position !== spaceBefore.position);
      await reload();
      check("the order is read back from the store after a reload", JSON.stringify(navRows().slice(0, 2)) === JSON.stringify(["workspace-empty", "workspace"]));
      const row = root.querySelector('[data-workspace-row="workspace-empty"] > button');
      row.focus(); row.dispatchEvent(new KeyboardEvent("keydown",{key:"ArrowDown",altKey:true,bubbles:true,cancelable:true}));
      await wait(() => navRows()[0] === "workspace");
      check("Alt+ArrowDown moves a workspace down from the keyboard and keeps the focus on it",
        navRows()[1] === "workspace-empty" && document.activeElement === root.querySelector('[data-workspace-row="workspace-empty"] > button'));
      await reload();
      check("and that order survives a reload too", JSON.stringify(navRows().slice(0, 2)) === JSON.stringify(["workspace", "workspace-empty"]));
    } catch (error) { check(`the workspace reorder checks ran to the end (${error.message})`, false); }

    // ---- Workspaces: importing the tab strip's groups ---------------------
    try {
      const named = name => workspaces.find(space => space.name === name);
      registerUnknown = true;
      await repoStore.openRepo("/fixture/GitPulse", { activate: false, group: "Agent tools" });
      await repoStore.openRepo("/fixture/Manvi", { activate: false, group: "Agent tools" });
      await repoStore.openRepo("/fixture/Research", { activate: false, group: "Research" });
      await repoStore.openRepo("/fixture/Notes", { activate: false, group: "developer tools" });
      await repoStore.openRepo("/fixture/Loose", { activate: false });
      await settle(200);
      const importButton = () => root.querySelector('[data-testid="import-tab-groups"]');
      check("the navigator offers to import every named tab group", importButton()?.getAttribute("aria-label") === "Import 3 tab groups as workspaces");
      const writesBefore = workspaceWrites.length;
      importButton().click();
      await wait(() => Boolean(named("Agent tools")) && Boolean(named("Research")));
      await settle(200);
      check("a tab group becomes a workspace holding each of its repositories once",
        JSON.stringify(named("Agent tools").repository_ids) === JSON.stringify(["repo-0", "repo-1"]) && named("Research").repository_ids.length === 1);
      check("a group whose name a workspace already has is skipped, never written into",
        workspaces.filter(space => space.name.toLowerCase() === "developer tools").length === 1
        && workspaceWrites.slice(writesBefore).every(write => write.id !== "workspace")
        && root.textContent.toLowerCase().includes("skipped 1 group that already has a workspace"));
      const navNames = () => [...root.querySelectorAll('nav[aria-label="Task scopes"] [data-workspace-row] > button')].map(el => el.textContent.trim());
      check("imported workspaces join the navigator after the ones already arranged",
        navNames().findIndex(name => name.includes("Agent tools")) > navNames().findIndex(name => name.includes("Developer tools"))
        && navNames().findIndex(name => name.includes("Agent tools")) > navNames().findIndex(name => name.includes("Fresh space")));
      const created = workspaces.length;
      importButton().click(); await settle(300);
      check("importing again creates nothing: it is idempotent by name", workspaces.length === created);
      registerUnknown = false;
    } catch (error) { check(`the tab group import checks ran to the end (${error.message})`, false); }

    // ---- Relinking a repository whose checkout moved -----------------------
    try {
      const relinkButton = () => button("Relink GitPulse to a moved checkout");
      const relinkPrompt = () => [...document.querySelectorAll('[role="dialog"]')].find(node => node.getAttribute("aria-label") === "Relink GitPulse?");
      const linkedBefore = tasks.filter(task => task.repository_ids.includes("repo-0")).map(task => `${task.id}@${task.revision}`);
      pickFolderResult = null; relinkButton().click(); await settle(100);
      check("cancelling the folder picker relinks nothing", relinkCalls.length === 0 && !relinkPrompt());
      pickFolderResult = "/moved/GitPulse"; relinkButton().click(); await wait(() => relinkPrompt());
      check("the confirmation names where the repository was and where it goes",
        relinkPrompt().textContent.includes("/fixture/GitPulse") && relinkPrompt().textContent.includes("/moved/GitPulse") && relinkPrompt().textContent.includes("never merged"));
      button("Relink", relinkPrompt()).click();
      await wait(() => repos.find(repo => repo.id === "repo-0").identity_key === "local:/moved/GitPulse/.git");
      await settle(200);
      check("relinking keeps the repository's id and every task linked to it, untouched",
        relinkCalls.length === 1 && relinkCalls[0].expectedRevision === 1
        && JSON.stringify(tasks.filter(task => task.repository_ids.includes("repo-0")).map(task => `${task.id}@${task.revision}`)) === JSON.stringify(linkedBefore));
      button("GitPulse").click(); await settle(400);
      check("the repository's board still shows its tasks after the relink", Boolean(card("task-2")));
      // A lost reply leaves a retry that sends the same request again.
      loseRelink = true; pickFolderResult = "/moved-again/GitPulse";
      relinkButton().click(); await wait(() => relinkPrompt()); button("Relink", relinkPrompt()).click();
      await wait(() => root.querySelector('[data-testid="repository-relink-uncertain"]'));
      check("an uncertain relink says so and offers a retry", Boolean(button("Retry relink")));
      button("Retry relink").click();
      await wait(() => !root.querySelector('[data-testid="repository-relink-uncertain"]'));
      check("the retry reuses the request and relinks once", relinkCalls.length === 3 && relinkCalls[1].requestId === relinkCalls[2].requestId
        && repos.find(repo => repo.id === "repo-0").revision === 3);
      await click("Global fixture"); await settle(200);
    } catch (error) { check(`the repository relink checks ran to the end (${error.message})`, false); }

    // ---- Unsaved suggestion edits are asked about on every board route -----
    // Editing a suggestion holds the assist busy, and the sheet used to treat
    // that busy as "cannot leave" without a word: every route below did
    // nothing, and the close button was disabled. Each must now ask.
    const nowSec = () => Math.floor(Date.now() / 1000);
    const proposalFor = (taskId, id, extra = {}) => {
      const source = structuredClone(tasks.find(task => task.id === taskId));
      return { id, revision: 1, updated_at: 1, task_id: taskId, source_revision: source.revision, source, fields: ["title", "description"], state: "ready", provider: "local", model: "quick-fixture", automatic: false, created_at: nowSec(), expires_at: nowSec() + 3600, failure: "", accepted_fields: [], edited_fields: [], outcome_uncertain: false, proposed: { title: "Guarded title", description: "Guarded description" }, rationale: "", ...extra };
    };
    const openAssist = async () => { if (assistToggle() && assistToggle().getAttribute("aria-expanded") === "false") { assistToggle().click(); await settle(100); } };
    const revisedTitle = (within = root) => [...within.querySelectorAll("textarea")].find(el => el.closest("label")?.textContent.trim().startsWith("Revised title"));
    let guardStep = "start";
    const showAllScopes = async () => { button("All", root.querySelector('nav[aria-label="Task scopes"]')).click(); await wait(() => card("task-3") && card("task-1")); };
    try {
      // The relink checks leave the board on one repository's scope.
      guardStep = "scopes";
      await showAllScopes();
      proposals.set("enhancement-guard", proposalFor("task-3", "enhancement-guard"));
      card("task-1").click(); await wait(editor);
      guardStep = "open task-3";
      card("task-3").click(); await wait(() => editor()?.querySelector("input[name=task-title]")?.value === "Review the agent handoff");
      guardStep = "find Edit suggestion";
      await openAssist();
      await wait(() => button("Edit suggestion", editor()));
      button("Edit suggestion", editor()).click(); await settle();
      await change(revisedTitle(), "A revised title nobody saved");
      const stillEditing = () => revisedTitle()?.value === "A revised title nobody saved";
      const routes = [
        ["opening another card", () => card("task-1").click()],
        ["switching task tabs", () => root.querySelector('[data-task-tab="task-1"]').click()],
        ["the sheet's close button", () => button("Close task details").click()],
        ["Escape in the sheet", () => revisedTitle().dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }))],
        ["New task", () => button("New task").click()],
        ["New workspace", () => button("New workspace").click()],
        ["a terminal's back link to another task", () => requestTaskOpen("task-1")],
      ];
      confirmAnswer = false;
      for (const [name, go] of routes) {
        confirmations = 0;
        go(); await settle(200);
        check(`${name} asks before dropping unsaved suggestion edits, and Keep editing keeps them`, confirmations === 1 && stillEditing());
      }
      guardStep = "discard";
      confirmAnswer = true; confirmations = 0;
      card("task-1").click();
      await wait(() => root.querySelector('[data-task-tab="task-1"][aria-selected="true"]') && editor() && !revisedTitle());
      check("Discard edits on that prompt leaves for the task asked for", confirmations === 1 && !revisedTitle());
      await click("Close task details"); await settle(100);
      for (const tab of [...root.querySelectorAll('[data-testid="task-tab-close"]')]) { tab.click(); await settle(80); }

      // The same edits in Quick Enhance, which hosts the same assist.
      guardStep = "quick enhance";
      const sheet = () => document.querySelector('[aria-labelledby="quick-enhance-title"]');
      card("task-3").focus(); card("task-3").dispatchEvent(new KeyboardEvent("keydown", { key: "e", bubbles: true, cancelable: true }));
      await wait(() => sheet() && button("Edit suggestion", sheet()));
      button("Edit suggestion", sheet()).click(); await settle();
      await change(revisedTitle(sheet()), "A revised title nobody saved");
      const sheetEditing = () => Boolean(sheet()) && revisedTitle(sheet())?.value === "A revised title nobody saved";
      const sheetRoutes = [
        ["Quick Enhance's close button", () => button("Close Quick Enhance", sheet()).click()],
        ["Escape in Quick Enhance", () => revisedTitle(sheet()).dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }))],
        ["Quick Enhance's Open full editor", () => button("Open full editor", sheet()).click()],
        ["a terminal's back link while Quick Enhance is open", () => requestTaskOpen("task-1")],
      ];
      confirmAnswer = false;
      for (const [name, go] of sheetRoutes) {
        confirmations = 0;
        go(); await settle(200);
        check(`${name} asks before dropping unsaved suggestion edits`, confirmations === 1 && sheetEditing());
      }
      confirmAnswer = true; confirmations = 0;
      button("Close Quick Enhance", sheet()).click(); await wait(() => !sheet());
      check("Discard edits closes Quick Enhance", confirmations === 1);
      proposals.delete("enhancement-guard");
    } catch (error) { check(`the suggestion-edit route guard checks ran to the end (${error.message} at ${guardStep}; toggle=${assistToggle()?.getAttribute("aria-expanded")}; assist=${(editor()?.querySelector("[aria-label=\"Manvi task assist\"]")?.textContent ?? "none").replace(/\s+/g, " ").slice(0, 400)})`, false); }

    // ---- The newest active suggestion, wherever it sits in the history ---
    // Thirty attempts are listed per page. A run older than the first page
    // was never selected, never polled, and did not stop a second one.
    try {
      const pagedTask = "task-3";
      const at = nowSec() - 500;
      proposals.set("paged-live", proposalFor(pagedTask, "paged-live", { state: "running", worker_id: "worker", created_at: at, proposed: undefined }));
      for (let i = 0; i < 32; i++) proposals.set(`paged-${i}`, proposalFor(pagedTask, `paged-${i}`, { state: "dismissed", created_at: at + 1 + i }));
      const picker = () => editor()?.querySelector('[data-testid="task-assist-history"]');
      guardStep = "paged: scopes";
      await showAllScopes();
      guardStep = `paged: card ${Boolean(card(pagedTask))}`;
      card(pagedTask).click(); await wait(editor);
      guardStep = "paged: assist";
      await openAssist();
      await wait(() => picker() && picker().options.length >= 30);
      await settle(300);
      check("the review opens on the running attempt even though it is past the first page", picker().value === "paged-live" && (editor()?.querySelector('article[aria-label="Enhancement review"]')?.textContent ?? "").length > 0);
      const readsBefore = enhancementReads.filter(id => id === "paged-live").length;
      await settle(2200);
      check("and that attempt is polled while it runs", enhancementReads.filter(id => id === "paged-live").length >= readsBefore + 2);
      check("a second generation cannot start beside a live attempt on another page", enhanceButton()?.matches(":disabled") === true);
      guardStep = "paged: refresh";
      check("the picker lists the pages down to that attempt, so it can name it", picker().options.length === 33 && !button("Load more", editor()));
      await settle(1600);
      check("the history refresh a live attempt drives keeps the pages already loaded", picker().options.length === 33);
      proposals.set("paged-live", { ...proposals.get("paged-live"), state: "dismissed", revision: 2 });
      await settle(1200);
      await click("Close task details"); await settle(100);
      for (const tab of [...root.querySelectorAll('[data-testid="task-tab-close"]')]) { tab.click(); await settle(80); }
      for (const id of [...proposals.keys()]) if (id.startsWith("paged-")) proposals.delete(id);
    } catch (error) { check(`the paged suggestion history checks ran to the end (${error.message} at ${guardStep})`, false); }

    // ---- Checkouts that moved, and repositories with only a remote --------
    // The board header's own Refresh: other panes carry a button of that name.
    const boardRefresh = () => root.querySelector('header [aria-label="Refresh"]');
    const refreshBoard = async () => { await wait(() => boardRefresh() && !boardRefresh().disabled); boardRefresh().click(); await settle(150); await wait(() => !boardRefresh().disabled); };
    try {
      guardStep = "checkouts: scopes";
      await showAllScopes();
      const row = id => root.querySelector(`[data-repository-row="${id}"]`);
      const chip = id => card(id)?.querySelector('[data-testid="card-checkout"]');
      const relinkPrompt = name => [...document.querySelectorAll('[role="dialog"]')].find(node => node.getAttribute("aria-label") === `Relink ${name}?`);
      missingCheckouts.add("/fixture/Manvi");
      repos.push({ id: "repo-remote", name: "Docs site", revision: 1, updated_at: 1, identity_key: "remote:github.com/fixture/docs", remote_url: "https://github.com/fixture/docs.git" });
      tasks.push(makeTask("task-90", "Publish the docs site", "repo-remote", "backlog"));
      await refreshBoard();
      guardStep = "checkouts: missing";
      await wait(() => row("repo-1")?.dataset.checkout === "missing" && row("repo-remote"));
      check("a moved checkout is marked on its navigator row before anyone relinks", row("repo-1").textContent.includes("Missing") && row("repo-0").dataset.checkout === "available");
      check("the mark says where the checkout was expected", row("repo-1").querySelector("button").title.includes("/fixture/Manvi"));
      check("a card whose repository's checkout is missing says so, before any launch", chip("task-3")?.textContent.includes("Checkout missing") && !chip("task-1"));
      check("a remote-only repository is listed, marked, and offers to link a local checkout",
        row("repo-remote").dataset.checkout === "remote" && row("repo-remote").textContent.includes("Remote only")
        && Boolean(button("Link Docs site to a local checkout")) && !button("Relink Docs site to a moved checkout"));
      check("its tasks carry no local path: the card says Remote only", chip("task-90")?.textContent.includes("Remote only"));
      // A check that could not run is neither available nor missing.
      const gitpulsePath = repos.find(repo => repo.id === "repo-0").identity_key.replace(/^local:/, "").replace(/\/\.git$/, "");
      uncheckableCheckouts.add(gitpulsePath);
      await refreshBoard();
      await wait(() => row("repo-0")?.dataset.checkout === "unknown");
      check("a checkout that could not be checked says so instead of passing or failing",
        row("repo-0").textContent.includes("Not checked") && !chip("task-1")
        && (root.querySelector('[data-testid="checkout-unchecked"]')?.textContent ?? "").includes("Operation not permitted"));
      uncheckableCheckouts.clear();
      guardStep = "checkouts: relink";
      pickFolderResult = "/relocated/Manvi";
      button("Relink Manvi to a moved checkout").click(); await wait(() => relinkPrompt("Manvi"));
      button("Relink", relinkPrompt("Manvi")).click();
      await wait(() => row("repo-1")?.dataset.checkout === "available");
      check("relinking clears the mark from the row and the cards", !row("repo-1").textContent.includes("Missing") && !chip("task-3"));
      pickFolderResult = "/fixture/Docs";
      button("Link Docs site to a local checkout").click(); await wait(() => relinkPrompt("Docs site"));
      check("linking a remote-only repository says it had no local checkout", relinkPrompt("Docs site").textContent.includes("had no local checkout"));
      button("Relink", relinkPrompt("Docs site")).click();
      await wait(() => row("repo-remote")?.dataset.checkout === "available");
      check("once linked it is an ordinary local repository, with its task still on it", !chip("task-90") && tasks.find(task => task.id === "task-90").repository_ids[0] === "repo-remote");
      missingCheckouts.clear();
      tasks = tasks.filter(task => task.id !== "task-90");
      repos.splice(repos.findIndex(repo => repo.id === "repo-remote"), 1);
      await refreshBoard(); await wait(() => !row("repo-remote"));
    } catch (error) { check(`the checkout checks ran to the end (${error.message} at ${guardStep}; rows=${[...root.querySelectorAll("[data-repository-row]")].map(el => `${el.dataset.repositoryRow}:${el.dataset.checkout}:${el.querySelector("button")?.title}`).join(" | ")}; unchecked=${root.querySelector('[data-testid="checkout-unchecked"]')?.textContent})`, false); }

    // ---- Every page of a column, in both layouts -------------------------
    try {
      guardStep = "paging";
      for (let i = 0; i < 35; i++) tasks.push(makeTask(`task-${300 + i}`, `Paged review ${String(i + 1).padStart(2, "0")}`, "repo-0", "review"));
      await refreshBoard();
      button("List").click();
      await wait(() => root.querySelector('[data-task-list] [data-task-paging="review"]'));
      const paging = () => root.querySelector('[data-task-list] [data-task-paging="review"]');
      const pagedRows = () => [...root.querySelectorAll("[data-task-list] [data-task-card]")].filter(el => el.textContent.includes("Paged review")).length;
      const before = pagedRows();
      check("the list layout pages a column too, and says how much of it is loaded", /^Review: 30 of \d+/.test(paging().querySelector(".paging-count").textContent.trim()) && before < 35);
      button("Load more Review tasks").click();
      await wait(() => pagedRows() === 35);
      check("Load more in the list reaches every task in the column", !button("Load more Review tasks") && pagedRows() === 35);
      button("Show only the first Review page").click();
      await wait(() => pagedRows() === before);
      check("First page goes back to one page", pagedRows() === before);
      button("Board").click(); await settle(200);
      const review = () => root.querySelector('[data-task-column="review"]');
      check("the board's column names its control for what it does", Boolean(button("Load more Review tasks", review())) && !button("Next", review()));
      tasks = tasks.filter(task => !task.title.startsWith("Paged review"));
      await refreshBoard(); await settle(200);
    } catch (error) { check(`the paging checks ran to the end (${error.message} at ${guardStep})`, false); }

    // ---- Work-in-progress limits, lanes and saved views, per board --------
    try {
      guardStep = "views: scopes";
      await showAllScopes();
      const viewMenu = () => document.querySelector('[data-testid="task-view-menu"]');
      const openView = async () => { if (!viewMenu()) { root.querySelector("[data-task-view-toggle]").click(); await settle(); } return viewMenu(); };
      const closeView = async () => { if (viewMenu()) { root.querySelector("[data-task-view-toggle]").click(); await settle(); } };
      const column = status => root.querySelector(`[data-task-column="${status}"]`);
      const head = status => root.querySelector(`[data-task-column-head="${status}"]`) ?? column(status);
      const wipInput = status => viewMenu().querySelector(`[data-task-wip-input="${status}"]`);
      // The column counts what the board reads: live tasks that are not archived.
      // The archive block above files Ready tasks away without moving them.
      const readyTotal = () => tasks.filter(task => !deleted.has(task.id) && !task.archived && task.status === "ready").length;

      guardStep = "views: wip";
      await openView();
      const limit = readyTotal() - 1;
      await change(wipInput("ready"), String(limit), "change");
      await closeView();
      const loadedReady = column("ready").querySelectorAll("[data-task-card]").length;
      check("a column over its work-in-progress limit is marked over it, in its head",
        column("ready").dataset.wip === "over" && Boolean(column("ready").querySelector('[data-testid="task-column-over"]'))
        && column("ready").querySelector('[data-testid="task-column-count"]').textContent.trim() === `${readyTotal()}/${limit}`);
      check("over is judged on the column's total, not on the page loaded", loadedReady <= limit || readyTotal() <= 30);
      check("a column with no limit carries no mark", !column("review")?.dataset.wip && !column("review")?.querySelector('[data-testid="task-column-over"]'));
      await openView(); await change(wipInput("ready"), String(readyTotal()), "change"); await closeView();
      check("exactly at the limit is not over it", column("ready").dataset.wip === "at" && !column("ready").querySelector('[data-testid="task-column-over"]'));
      await openView(); await change(wipInput("ready"), "0", "change");
      check("a limit the board cannot hold is refused, and the box says none", wipInput("ready").value === "" && !get(interfaceStore).taskBoards.global);
      await change(wipInput("ready"), String(limit), "change"); await closeView();
      workspaceTab("Developer tools").click(); await settle(400);
      await openView();
      check("limits belong to one board: the workspace's board has none", wipInput("ready").value === "" && get(interfaceStore).taskBoards.global?.wip.ready === limit);
      await closeView();
      await showAllScopes(); await settle(200);
      check("and the global board still has its own", column("ready").dataset.wip === "over");
      await openView(); await change(wipInput("ready"), "", "change"); await closeView();
      check("clearing the limit clears the mark", !column("ready").dataset.wip);

      guardStep = "views: lanes";
      await openView();
      viewMenu().querySelector('[data-task-lane-option="owner"]').click(); await settle(200);
      await closeView();
      const lanes = () => [...root.querySelectorAll(".columns [data-task-lane]")];
      const laneOf = id => card(id)?.closest("[data-task-lane]")?.dataset.taskLane;
      const ownerOf2 = tasks.find(task => task.id === "task-2").owner;
      check("owner lanes put each card in its owner's lane, unassigned last",
        lanes().length >= 2 && laneOf("task-2") === `owner:${ownerOf2}` && lanes().at(-1).dataset.taskLane === "owner:");
      const drawnIds = [...root.querySelectorAll(".columns [data-task-card]")].map(el => el.dataset.cardId);
      check("every card is drawn once across the lanes", drawnIds.length > 0 && new Set(drawnIds).size === drawnIds.length);
      const heads = root.querySelectorAll("[data-task-column-head]").length;
      check("the column heads, with counts and limits, are drawn once above the lanes",
        heads > 0 && lanes().every(lane => lane.querySelectorAll("[data-task-column]").length === heads));
      guardStep = `views: lane move from ${card("task-2")?.closest("[data-task-column]")?.dataset.taskColumn}`;
      const order = ["inbox", "backlog", "ready", "in_progress", "review", "done"];
      const from = card("task-2").closest("[data-task-column]").dataset.taskColumn, to = order[order.indexOf(from) - 1];
      const before = writes.length;
      card("task-2").focus(); card("task-2").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true, cancelable: true }));
      await wait(() => writes.length > before && card("task-2")?.closest("[data-task-column]")?.dataset.taskColumn === to);
      check("a keyboard move in a lane changes the status and keeps the card in its lane", writes.at(-1).status === to && writes.at(-1).owner === ownerOf2 && laneOf("task-2") === `owner:${ownerOf2}`);
      guardStep = `views: lane move back; now ${card("task-2")?.closest("[data-task-column]")?.dataset.taskColumn} writes=${writes.length}`;
      card("task-2").focus(); card("task-2").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
      await wait(() => tasks.find(task => task.id === "task-2").status === from);
      await openView();
      viewMenu().querySelector('[data-task-lane-option="status"]').click(); await settle(200);
      check("status lanes are not drawn on the board, and the menu says why", lanes().length === 0 && Boolean(viewMenu().querySelector('[data-testid="task-lanes-note"]')));
      await closeView();
      button("List").click(); await settle(200);
      const listLanes = [...root.querySelectorAll("[data-task-list] [data-task-lane]")].map(el => el.dataset.taskLane);
      check("in the list, status lanes group the rows by status", listLanes.length >= 2 && [...root.querySelectorAll("[data-task-list] [data-task-card]")].every(row => row.closest("[data-task-lane]")?.dataset.taskLane === `status:${tasks.find(task => task.id === row.dataset.cardId)?.status}`));
      button("Board").click(); await settle(150);

      guardStep = "views: saved";
      const showAll = async () => { const all = button("All", root.querySelector('nav[aria-label="Task scopes"]')); all.click(); await wait(() => all.getAttribute("aria-pressed") === "true"); await settle(400); };
      const viewsPanel = () => document.querySelector('[data-testid="task-saved-views"]');
      const viewsToggle = () => root.querySelector("[data-task-views-toggle]");
      const openViews = async () => { if (!viewsPanel()) { viewsToggle().click(); await settle(); } return viewsPanel(); };
      const closeViews = async () => { if (viewsPanel()) { viewsToggle().click(); await settle(); } };
      const saveAs = async name => { await openViews(); await change(viewsPanel().querySelector('input[aria-label="View name"]'), name); button("Save", viewsPanel()).click(); await settle(); };
      await openView(); viewMenu().querySelector('[data-task-lane-option="owner"]').click(); await settle(); await closeView();
      if (!root.querySelector('[aria-label="Filter by priority"]')) { await click("Filters"); }
      await change(root.querySelector('[aria-label="Filter by priority"]'), "1", "change");
      await saveAs("Owners");
      check("saving a view names it on the control and keeps it for this board", viewsToggle().textContent.trim() === "Owners"
        && get(interfaceStore).taskBoards.global?.views.some(view => view.name === "Owners" && view.swimlane === "owner" && view.facet.priority === 1));
      check("a saved view survives a reload: it is in the stored preferences", JSON.stringify({ ...localStorage }).includes("Owners"));
      await closeViews();
      await openView(); viewMenu().querySelector('[data-task-lane-option="none"]').click(); await settle(); await closeView();
      button("List").click(); await settle(150);
      await click("Clear filters");
      check("changing what is on screen leaves the saved view, and the control stops naming it", viewsToggle().textContent.trim() === "Views");
      await openViews();
      button("Owners", viewsPanel()).click(); await settle(300);
      check("applying a view puts back its layout, lanes and filters",
        get(interfaceStore).taskLayout === "board" && get(interfaceStore).taskSwimlane === "owner"
        && root.querySelector('[aria-label="Filter by priority"]')?.value === "1" && lanes().length >= 1 && viewsToggle().textContent.trim() === "Owners");
      await closeViews();
      workspaceTab("Developer tools").click(); await settle(400);
      await openViews();
      check("views belong to one board: the workspace's board lists none of the global board's", !button("Owners", viewsPanel()) && viewsPanel().textContent.includes("None yet"));
      await saveAs("Workspace view");
      await closeViews();
      await showAll();
      await openViews();
      check("and the global board does not list the workspace's", Boolean(button("Owners", viewsPanel())) && !button("Workspace view", viewsPanel()));
      viewsPanel().querySelector('[aria-label="Delete view Owners"]').click(); await settle();
      check("deleting a view removes it from this board only", !button("Owners", viewsPanel())
        && get(interfaceStore).taskBoards["workspace:workspace"]?.views.some(view => view.name === "Workspace view"));
      await closeViews();
      // Leave the board as the next checks expect it.
      interfaceStore.deleteTaskView("workspace:workspace", get(interfaceStore).taskBoards["workspace:workspace"]?.views[0]?.id ?? "");
      interfaceStore.setTaskSwimlane("none");
      await click("Clear filters"); await settle(200);
    } catch (error) { check(`the board view checks ran to the end (${error.message} at ${guardStep}; error=${root.querySelector(".banner.error")?.textContent ?? ""})`, false); }

    // ---- A repository's board reaches the global board -------------------
    try {
      guardStep = "global nav";
      await click("GitPulse fixture"); await wait(() => root.querySelector('[data-testid="task-board-all"]'));
      interfaceStore.setGlobalSurface("repository");
      root.querySelector('[data-testid="task-board-all"]').click(); await settle();
      check("a repository's board opens the Tasks board for everything", get(interfaceStore).globalSurface === "tasks");
      await click("Global fixture"); await wait(() => !root.querySelector('[data-testid="task-board-all"]'));
      check("the global board does not offer a link to itself", !root.querySelector('[data-testid="task-board-all"]'));
    } catch (error) { check(`the global navigation checks ran to the end (${error.message} at ${guardStep})`, false); }

    check(`no runtime errors or unconfigured fixture requests occurred${crashes.length || unknown.length ? ` (${[...crashes, ...unknown].join("; ")})` : ""}`, crashes.length === 0 && unknown.length === 0);
  } catch(error) { results.push({name:error.message, stack:error.stack, pass:false}); }
  document.getElementById("verdict").textContent = JSON.stringify({results}, null, 2);
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({results})));
  if(params.has("report")) await fetch(params.get("report"), {method:"POST", body:document.documentElement.outerHTML});
}
