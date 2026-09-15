import "../src/app.css";
import { mount, tick } from "svelte";
import { get } from "svelte/store";
import { promptState } from "../src/lib/stores/modalStore";
import { mockIPC } from "@tauri-apps/api/mocks";
import { applyPlatformClass } from "../src/lib/platform";
import { shortcutTextLabel } from "../src/lib/ui/platformCopy";
import { hostPlatform } from "../src/lib/stores/platformStore";
import TasksHost from "./TasksHost.svelte";
import { themeStore } from "../src/lib/stores/themeStore";
import { harnessStore } from "../src/lib/stores/harnessStore";

const params = new URLSearchParams(location.search);
const results = [], crashes = [], writes = [], unknown = [];
const preparedRuns = [], appleDrafts = [];
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
const makeTask = (id, title, repository, status = "ready") => ({ id, title, kind: "feature", status, priority: 2, severity: null, owner: null, due_at: null, labels: [], repository_ids: [repository], primary_repository_id: repository, home_workspace_id: null, position: Number(id.replace(/\D/g, "")) || 1, revision: 1, updated_at: 1, description: "Keep changes focused and verify the result.", acceptance_criteria: [], locked_fields: [] });
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
const deleted = new Set(), deleteWrites = [];
let failList = false, corruptSave = false, loseSave = false, holdSave = false, releaseSave, holdSearch = false, heldSearch = [], holdGet = false, releaseGet;
const receipts = new Map(), proposals = new Map(), enhancementWrites = [];
let failConfiguration = false, blankConfiguration = false, loseEnhancement = false, holdDelete = false, releaseDelete;
const page = (items, start = 0, limit = 200) => ({ ok: true, items: items.slice(start, start + limit), total: items.length, shown: items.slice(start, start + limit).length, has_more: start + limit < items.length, next_cursor: start + limit < items.length ? String(start + limit) : null });
mockIPC(async (cmd, args) => {
  if (cmd === "cmd_workbench_register_repository") return JSON.stringify({ repository: repos.find(repo => repo.identity_key === `local:${args.repoPath}/.git`) });
  if (cmd === "cmd_pick_folder") return null;
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
    case "runs.list": return JSON.stringify({ok:true, items:[], shown:0, total:0, has_more:false, next_cursor:null});
    case "runs.prepare_terminal": case "runs.prepare_managed": {
      preparedRuns.push(structuredClone(input));
      throw {code:"store_error", message:"The tasks fixture does not start agents."};
    }
    case "workspaces.list": return JSON.stringify(page(workspaces));
    case "workspaces.get": return JSON.stringify({ok: true, item: workspaces.find(space => space.id === input.id) ?? workspace});
    case "workspaces.put": {
      workspaceWrites.push(structuredClone(input));
      const index = workspaces.findIndex(space => space.id === input.id);
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
    case "enhancements.list": return JSON.stringify(page([...proposals.values()].filter(p=>p.task_id===input.task_id)));
    case "enhancements.get": return JSON.stringify({ok:true,item:proposals.get(input.id)});
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
      const matching = tasks.filter(task => !deleted.has(task.id) && task.status === input.status && (!input.repository_id || task.repository_ids.includes(input.repository_id)) && (!input.workspace_id || task.repository_ids.some(id => workspace.repository_ids.includes(id))) && (!input.query || task.title.toLowerCase().includes(input.query.toLowerCase())))
        // `ORDER BY t.position,t.id`, the same key the store pages on. The
        // fixture used to page in array order, so `items.put` — which appends
        // — moved an edited task to the end of its new column and off the
        // first page. Paging is what this fixture is for, so the order it
        // pages in has to be the real one.
        .sort((a, b) => a.position - b.position || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      const result = JSON.stringify(page(matching, Number(input.cursor ?? 0), input.limit));
      if (holdSearch && input.query === "older") return new Promise(resolve => heldSearch.push(() => resolve(result)));
      return result;
    }
    case "items.get": { const result = JSON.stringify({ok: true, item: tasks.find(task => task.id === input.id)}); if (holdGet) return new Promise(resolve => { releaseGet = () => resolve(result); }); return result; }
    case "items.delete": {
      deleteWrites.push(structuredClone(input));
      if(holdDelete) await new Promise(resolve=>{releaseDelete=resolve;});
      if (receipts.has(input.request_id)) return receipts.get(input.request_id);
      const previous = tasks.find(task => task.id === input.id);
      if (!previous || deleted.has(input.id) || previous.revision !== input.expected_revision) throw {code:"revision_conflict", message:"Task changed before deletion."};
      deleted.add(input.id);
      const result = JSON.stringify({ok:true, item:{...previous, deleted:true, revision:previous.revision + 1}});
      receipts.set(input.request_id, result);
      if (corruptDelete) { corruptDelete = false; return JSON.stringify({ok:false}); }
      if (loseDelete) { loseDelete = false; throw {code:"transport_error", message:"Lost delete reply"}; }
      return result;
    }
    case "items.put": {
      writes.push(structuredClone(input));
      if (holdSave) await new Promise(resolve => { releaseSave = resolve; });
      if (receipts.has(input.request_id)) return receipts.get(input.request_id);
      const previous = tasks.find(task => task.id === input.id);
      if ((previous?.revision ?? 0) !== input.expected_revision) throw {code:"conflict", message:"Task changed. Reload the saved task."};
      const { expected_revision, request_id, ...draft } = input;
      const item = {...draft, due_at: draft.due_at ?? null, revision: expected_revision + 1, updated_at: 2};
      tasks = [...tasks.filter(task => task.id !== item.id), item];
      const result = JSON.stringify({ok:true, item}); receipts.set(request_id, result);
      if (corruptSave) { corruptSave = false; return JSON.stringify({ok:true, item:{}}); }
      if (loseSave) { loseSave = false; throw {code:"transport_error", message:"Fixture lost the save reply"}; }
      return result;
    }
    default: unknown.push(args.method); throw Error(`Unconfigured method: ${args.method}`);
  }
}, {shouldMockEvents: true});
applyPlatformClass();
themeStore.setTheme(params.get("theme") === "light" ? "light" : "dark");
void harnessStore.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "quick-fixture" });
window.addEventListener("error", e => crashes.push(e.message));
window.addEventListener("unhandledrejection", e => crashes.push(String(e.reason)));
const root = document.getElementById("app");
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
const wait = async predicate => { const deadline = Date.now() + 15_000; while (Date.now() < deadline) { if (predicate()) return; await settle(); } throw Error("Timed out waiting for task UI"); };
const aliases = {"Quick Enhance":"Quick Enhance…","Add task to Ready":"New task in Ready","Close workspace details":"Close workspace settings", "Refresh tasks":"Refresh", "List view":"List", "Board view":"Board", "Duplicate task…":"Duplicate…", "Delete task":"Delete", "Retry deletion":"Retry delete"};
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
const repoPopup = () => document.querySelector("[data-task-repo-popup]");
const openRepoPicker = async () => { if (!repoPopup()) repoToggle()?.click(); await settle(80); return repoPopup(); };
// Dismissed the way a reader dismisses it: a pointerdown outside. The picker
// listens on the capture phase, so this reaches it from `body`.
const closeRepoPicker = async () => { if (repoPopup()) document.body.dispatchEvent(new PointerEvent("pointerdown", {bubbles:true})); await settle(60); };
const repoRow = name => [...(repoPopup()?.querySelectorAll(".repo-row") ?? [])].find(row => row.querySelector(".repo-name")?.textContent.trim() === name);
const repoSummaryText = () => repoToggle()?.textContent ?? "";
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
    const onScreen = el => Boolean(el) && el.getClientRects().length > 0;
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
      onScreen(editor().querySelector('[aria-label="Manvi task assist"]'))
      && !onScreen(editor().querySelector('[aria-label="Task agent runs"]')));
    // Schedule and labels are on this pane too, beside the description rather
    // than behind a second tab.
    check("the merged pane carries schedule and labels beside the task text",
      onScreen(field("Title")) && onScreen(field("Owner")) && onScreen(field("Due")) && onScreen(field("Labels")));
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
      onScreen(editor().querySelector(".manvi-assist")) && Boolean(historyPicker())
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
    await click("Close task details");

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
    const workspaceTab = name => [...root.querySelectorAll('nav[aria-label="Task scopes"] button')].find(el => el.textContent.trim().startsWith(name));
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
    await click("Delete selected tasks"); dialog=document.querySelector('[aria-label="Delete tasks"]');
    tasks.find(task=>task.id==="task-13").revision++; holdDelete=true;
    button("Delete 2 tasks",dialog).click(); await wait(()=>releaseDelete);
    check("in-flight deletion cannot close or submit twice", button("Close task action",dialog).disabled && !button("Delete 2 tasks",dialog));
    dialog.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();
    check("Escape retains the in-flight deletion dialog", document.querySelector('[aria-label="Delete tasks"]')===dialog);
    holdDelete=false; releaseDelete(); await wait(()=>dialog.textContent.includes("1 of 2 deleted"));
    check("mixed batch deletion preserves concurrent edits and reports partial failure", deleted.has("task-12") && !deleted.has("task-13") && dialog.textContent.includes("1 not changed"));
    button("Done",dialog).click(); await settle(); await click("Clear task selection");

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

    await change(field("Due"),"2026-09-01T09:30"); await click("Save task"); await settle(100);
    check("due dates round-trip through the task editor", field("Due").value==="2026-09-01T09:30");
    editor().querySelector('[data-sheet-tab="agent"]').click(); await settle();
    await change(field("Owner"),"ada"); await click("Save task"); await settle(100);
    check("saving keeps the reader on the pane they were editing", editor().querySelector('[data-sheet-tab="agent"]').getAttribute("aria-selected")==="true");
    editor().querySelector('[data-sheet-tab="task"]').click(); await settle();
    await change(field("Description"),"Unsaved context");
    check("unsaved edits stay local until Manvi prepare saves", editor().querySelector("header small").textContent==="Unsaved" && Boolean(enhanceButton()) && !enhanceButton().matches(":disabled"));
    confirmAnswer=true; await click("Close task details");
    await click("New task");
    const idea=root.querySelector(".notes-label textarea");
    check("new tasks start with one idea field and the current repository", Boolean(idea) && idea.getClientRects().length>0 && Boolean(button("Improve with Manvi")) && !editor().querySelector('.sheet-tabs'));
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
    check("inline suggestions own acceptance, and the review still shows the diff",
      (() => {
        const article = editor().querySelector('article[aria-label="Enhancement review"]');
        if (!article) return false;
        const inline = Boolean(button("Use both") || button("Use this title") || button("Use this description"));
        if (!inline) return true;
        return !button("Accept selected fields", article) && article.querySelectorAll('input[type="checkbox"]').length === 0;
      })());
    proposals.set(quickProposal.id,{...quickProposal,revision:quickProposal.revision+1,state:"ready",proposed:{title:"Reliable task notifications",description:"Preserve saved evidence, route notifications to the correct task, and verify recovery."}});
    await wait(()=>button("Use both"));
    await change(field("Description"),"Unsaved evidence that must survive");
    check("inline acceptance refuses to overwrite unsaved task edits", button("Use both").matches(":disabled"));
    await change(field("Description"),quickProposal.source.description);
    // Reload the saved revision so this acceptance starts from clean state.
    await click("Reload saved"); await settle(100);
    loseEnhancement=true; await click("Use both");
    const retryInline=button("Retry pending action");
    check("lost inline acceptance retains a retry and blocks closing", Boolean(retryInline) && button("Close task details").disabled);
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
    root.querySelector('[data-task-column="inbox"]')?.focus();
    document.dispatchEvent(new KeyboardEvent("keydown", {key:"a", bubbles:true}));
    await settle();
    check("the a shortcut puts the cursor in quick add", document.activeElement === quickAdd());
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
    // checks move tasks into and out of Done, so a hard-coded total would
    // measure this block's position in the script, not the archive.
    const completed = () => tasks.filter(task => !deleted.has(task.id) && task.status === "done").length;
    const expected = completed();
    check("the board badges the server's completed total, not a page of it",
      expected > 30 && archiveToggle()?.textContent.trim() === String(expected));
    archiveToggle().click(); await settle(200);
    await wait(() => archiveRows().length > 0);
    check("the archive opens on the completed tasks for this scope",
      Boolean(archive()) && archiveRows().length === 30);
    // The panel is named Archive and offers Restore. Until this line it never
    // said anywhere what puts a task in it, and the one place that came close
    // was the empty state — the single case a reader has no archived work to
    // ask about.
    check("the archive says how a task gets into it, with rows on screen",
      archive().querySelector('[data-testid="task-archive-rule"]')?.textContent.trim()
        === "A task is archived when it reaches Done.");
    // The one number this panel must not get wrong. 30 rows on screen out of
    // 35 completed tasks has to read as both numbers, or a reader clears an
    // archive they have only partly seen.
    check("a partly loaded archive says so, with both numbers",
      archiveSummaryText() === `Showing 30 of ${expected} completed tasks. Load more to see the rest.`);
    button("Load more", archive()).click(); await settle(200);
    check("Load more grows the page instead of replacing it",
      archiveRows().length === expected && archiveSummaryText() === `${expected} completed tasks.`);
    // A restore is the board's own status update, so it must land in the
    // board's confirm dialog rather than writing straight through.
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
    await change(archive().querySelector('[aria-label="Restore to"]'), "ready", "change");
    button("Restore", archive()).click(); await settle(150);
    const restoreDialog = document.querySelector('[role="dialog"][aria-label="Update tasks"]');
    check("restoring goes through the board's confirm-and-retry dialog", Boolean(restoreDialog));
    button("Update 2 tasks", restoreDialog).click(); await settle(250);
    button("Done", restoreDialog).click(); await settle(250);
    check("a restored task leaves the archive and returns to the chosen column",
      restored.every(id => tasks.find(task => task.id === id)?.status === "ready")
      && archiveRows().every(row => !restored.includes(row.getAttribute("data-card-id")))
      && restored.every(id => Boolean(card(id)?.closest('[data-task-column="ready"]'))));
    // A write drops the accumulated pages and reloads from the first one.
    // Merging a fresh page into rows fetched before the write is how a
    // restored or deleted task keeps its seat in the list; the cursor only
    // runs forward, so there is no way to re-fetch the deeper pages. The
    // summary then has to report the new total against the 30 rows actually
    // reloaded, which is the case this asserts.
    check("a restore reloads the archive from its first page, against the new total",
      archiveToggle().textContent.trim() === String(expected - 2)
      && archiveRows().length === Math.min(30, expected - 2)
      && archiveSummaryText() === `Showing 30 of ${expected - 2} completed tasks. Load more to see the rest.`);
    await change(archive().querySelector('[aria-label="Search completed tasks"]'), "Completed task 30");
    await settle(350);
    check("the archive searches completed work without touching the board's search",
      archiveRows().length === 1 && root.querySelector('[aria-label="Search tasks"]').value === "");
    await change(archive().querySelector('[aria-label="Search completed tasks"]'), "");
    await settle(350);
    // The dock is a second way to read completed work, not a move, and it
    // says which of the two is true right now.
    check("the archive admits that Done is still on the board",
      archive().textContent.includes("in the Done column on the board") && Boolean(columnEl("done")));
    button("Hide Done on the board", archive()).click(); await settle(200);
    check("hiding Done from the archive uses the board's own column preference",
      !columnEl("done") && archive().textContent.includes("Done column is hidden"));
    check("the board's hidden-column note then offers the archive by name",
      Boolean(hiddenNote()) && Boolean(button("Open archive", hiddenNote())));
    button("Show Done on the board", archive()).click(); await settle(200);
    check("and the same control puts it back", Boolean(columnEl("done")) && !hiddenNote());
    // A backgrounded window defers the query, the way the Inbox does. What it
    // must not do is answer it: an unread archive saying "No completed tasks
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
      && !archiveSummaryText().includes("No completed tasks")
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
    // inside it, and no Archive verb anywhere in the product. The only route
    // was `Move to… › Done`, and nothing named the two as the same thing.
    const archiveRow = () => [...menu().querySelectorAll("button")].find(el => el.dataset.menuId === "archive");
    const statusOf = id => tasks.find(task => task.id === id)?.status;
    const revisionOf = id => tasks.find(task => task.id === id)?.revision;
    // Taken from the board as it stands, not written as literals: earlier
    // blocks in this script delete tasks and move others between columns, so
    // a hard-coded id would be measuring this block's position in the script.
    const onBoardIn = status => [...root.querySelectorAll(`[data-task-column="${status}"] [data-task-card]`)]
      .map(el => el.getAttribute("data-card-id"));
    const [first, second] = onBoardIn("ready");
    if (!first || !second) throw Error("Archive checks need two Ready cards on the board");
    await openMenu(first);
    check("a card's own menu offers Archive, and names the column it files into",
      Boolean(archiveRow()) && !archiveRow().disabled
      && archiveRow().textContent.includes("Archive") && archiveRow().textContent.includes("Done"));
    const beforeArchive = completed();
    archiveRow().click(); await settle(350);
    check("Archive moves the task without opening a dialog or asking for a status",
      statusOf(first) === "done" && !document.querySelector('[role="dialog"]')
      && Number(archiveToggle().textContent.trim()) === beforeArchive + 1
      && Boolean(card(first)?.closest('[data-task-column="done"]')));
    // Disabled rather than hidden, the way a Move row showing the current
    // status is: the menu keeps its shape, and says what the task already is.
    await openMenu(first);
    check("an already-archived task is told so instead of being written again",
      Boolean(archiveRow()) && archiveRow().disabled && archiveRow().textContent.includes("Already in Done"));
    menu().dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true})); await settle();

    // The bulk half, over a deliberately mixed selection: one task already in
    // Done and one that is not. Archiving both would spend a revision on the
    // archived one to store the status it already had, and hand the batch one
    // more write to fail on.
    if (root.querySelector(".selection")) await click("Clear task selection");
    await selectCard(first); await selectCard(second);
    const bulkButton = () => root.querySelector('[data-testid="task-archive-selected"]');
    const before = { archived: revisionOf(first), open: revisionOf(second) };
    check("the selection bar offers Archive beside Delete for a mixed selection",
      Boolean(bulkButton()) && !bulkButton().disabled
      && root.querySelector('[aria-label="Selected task actions"]').textContent.includes("2 selected"));
    bulkButton().click(); await settle(450);
    check("bulk Archive files the rest and leaves the already-archived task untouched",
      statusOf(first) === "done" && statusOf(second) === "done"
      && revisionOf(first) === before.archived
      && revisionOf(second) === before.open + 1);
    check("and it refuses once there is nothing left to archive",
      Boolean(bulkButton()) && bulkButton().disabled && bulkButton().title.includes("Already in Done"));
    await click("Clear task selection");

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

    // ---- Handing a card to an agent --------------------------------------
    await openMenu("task-11"); button("Send to agent…", menu()).click(); await settle();
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
    enhanceButton().click(); await wait(() => button("Use both"));
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
    check("an on-device suggestion is reviewed exactly like a Manvi one",
      Boolean(button("Use this title")) && Boolean(button("Use this description")) && Boolean(button("Not now")));
    await click("Use both"); await settle(150);
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

    check("no runtime errors or unconfigured fixture requests occurred", crashes.length === 0 && unknown.length === 0);
  } catch(error) { results.push({name:error.message, stack:error.stack, pass:false}); }
  document.getElementById("verdict").textContent = JSON.stringify({results}, null, 2);
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({results})));
  if(params.has("report")) await fetch(params.get("report"), {method:"POST", body:document.documentElement.outerHTML});
}
