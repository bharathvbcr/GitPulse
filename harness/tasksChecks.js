import "../src/app.css";
import { mount, tick } from "svelte";
import { get } from "svelte/store";
import { promptState } from "../src/lib/stores/modalStore";
import { mockIPC } from "@tauri-apps/api/mocks";
import { applyPlatformClass } from "../src/lib/platform";
import TasksHost from "./TasksHost.svelte";
import { themeStore } from "../src/lib/stores/themeStore";
import { harnessStore } from "../src/lib/stores/harnessStore";

const params = new URLSearchParams(location.search);
const results = [], crashes = [], writes = [], unknown = [];
const repos = ["GitPulse", "Manvi"].map((name, i) => ({ id: `repo-${i}`, name, revision: 1, updated_at: 1, identity_key: `/fixture/${name}`, remote_url: null }));
const workspace = { id: "workspace", name: "Developer tools", revision: 1, updated_at: 1, icon: "", color: "", pinned: false, archived: false, position: 1, description: "", repository_ids: ["repo-1"], repository_count: 1 };
const makeTask = (id, title, repository, status = "ready") => ({ id, title, kind: "feature", status, priority: 2, severity: null, owner: null, due_at: null, labels: [], repository_ids: [repository], primary_repository_id: repository, home_workspace_id: null, position: Number(id.replace(/\D/g, "")) || 1, revision: 1, updated_at: 1, description: "Keep changes focused and verify the result.", acceptance_criteria: [], locked_fields: [] });
let tasks = [
  makeTask("task-1", "Keep repository tasks in sync", "repo-0", "in_progress"),
  { ...makeTask("task-2", "Make task sheets easier to scan", "repo-0", "review"), priority: 1, kind: "improvement", owner: "Bharath", labels: ["usability", "tasks", "desktop"], repository_ids: ["repo-0", "repo-1"] },
  makeTask("task-3", "Review the agent handoff", "repo-1", "backlog"),
  ...Array.from({length: 32}, (_, i) => makeTask(`task-${i + 10}`, `Ready task ${String(i + 1).padStart(2, "0")}`, "repo-0")),
];
let corruptDelete = false, loseDelete = false;
const deleted = new Set(), deleteWrites = [];
let failList = false, corruptSave = false, loseSave = false, holdSave = false, releaseSave, holdSearch = false, heldSearch = [], holdGet = false, releaseGet;
const receipts = new Map(), proposals = new Map(), enhancementWrites = [];
let failConfiguration = false, blankConfiguration = false, loseEnhancement = false, holdDelete = false, releaseDelete;
const page = (items, start = 0, limit = 200) => ({ ok: true, items: items.slice(start, start + limit), total: items.length, shown: items.slice(start, start + limit).length, has_more: start + limit < items.length, next_cursor: start + limit < items.length ? String(start + limit) : null });
mockIPC(async (cmd, args) => {
  if (cmd === "cmd_workbench_register_repository") return JSON.stringify({ repository: repos.find(repo => repo.identity_key === args.repoPath) });
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
  if (cmd !== "cmd_workbench_request") { unknown.push(cmd); throw Error(`Unconfigured command: ${cmd}`); }
  const input = JSON.parse(args.input);
  switch (args.method) {
    case "repositories.list": return JSON.stringify(page(repos));
    case "workspaces.list": return JSON.stringify(page([workspace]));
    case "workspaces.get": return JSON.stringify({ok: true, item: workspace});
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
    case "enhancements.create": case "enhancements.generate": case "enhancements.accept": case "enhancements.undo": case "enhancements.dismiss": {
      enhancementWrites.push({method:args.method,...structuredClone(input)});
      if(receipts.has(input.request_id)) return receipts.get(input.request_id);
      let proposal = proposals.get(input.id);
      if(args.method === "enhancements.create") {
        const source = structuredClone(tasks.find(task=>task.id === input.task_id));
        proposal = {id:input.id,revision:1,updated_at:1,task_id:input.task_id,source_revision:source.revision,source,fields:input.fields,state:"pending",provider:input.provider,model:input.model,automatic:false,created_at:1,expires_at:Math.floor(Date.now()/1000)+60};
      } else {
        if(!proposal || proposal.revision !== input.expected_revision) throw {code:"revision_conflict",message:"Suggestion changed"};
        proposal = {...proposal,revision:proposal.revision+1};
        if(args.method === "enhancements.generate") { proposal.state="running"; proposal.worker_id="worker"; }
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
      const matching = tasks.filter(task => !deleted.has(task.id) && task.status === input.status && (!input.repository_id || task.repository_ids.includes(input.repository_id)) && (!input.workspace_id || task.repository_ids.some(id => workspace.repository_ids.includes(id))) && (!input.query || task.title.toLowerCase().includes(input.query.toLowerCase())));
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
const enhanceButton = () => button("Improve with Manvi") ?? button("Enhance with Manvi") ?? button("Draft with Manvi");
const card = id => root.querySelector(`[data-card-id="${id}"]`);
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
    check("the primary repository is available without expanding settings", field("Primary repository").getClientRects().length > 0 && !field("Primary repository").closest("details"));
    check("linked repositories remain intact in the draft", field("Primary repository").value === "repo-0" && editor().textContent.includes("Linked repositories"));
    check("optional Manvi history and advanced details are folded initially", editor().querySelector(".history-drawer")?.open !== true && button("More details").getAttribute("aria-checked") === "false");
    check("merged Manvi section has no model input", Boolean(editor().querySelector(".manvi-assist .change-link")) && ![...editor().querySelectorAll(".manvi-assist label")].some(label => label.firstChild?.textContent.trim() === "Model"));
    const stacked = (a, b) => a && b && a.getBoundingClientRect().bottom <= b.getBoundingClientRect().top + 2;
    check("saved-task sheet stacks assist above runs", stacked(editor().querySelector('[aria-label="Manvi task assist"]'), editor().querySelector('[aria-label="Task agent runs"]')));
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
    if (button("Schedule and labels")?.getAttribute("aria-checked") !== "true") await click("Schedule and labels");
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
    check("workspace tasks default to a repository in that workspace", field("Primary repository").value === "repo-1");
    await change(field("Title"), "Workspace draft");
    await click("GitPulse fixture"); await settle(400);
    check("repository tab switches preserve the open draft's repository", field("Title").value === "Workspace draft" && field("Primary repository").value === "repo-1");
    confirmAnswer = true; await click("Close task details");
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
    editor().querySelector(".history-drawer > summary")?.click(); await settle();
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
    if(button("Schedule and labels")?.getAttribute("aria-checked") !== "true") await click("Schedule and labels");
    await change(field("Due"),"2026-09-01T09:30"); await click("Save task"); await settle(100);
    check("due dates round-trip through the task editor", field("Due").value==="2026-09-01T09:30");
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
    const typeLabel = [...editor().querySelectorAll("label")].find(label => label.firstChild?.textContent.trim() === "Type");
    check("Manvi assist sits above task type controls", stacked(assistBox, typeLabel));
    check("one click saves the rough idea and starts Manvi with both fields", quickProposal.fields.length===2 && quickProposal.source.description.includes("explain recovery steps") && quickProposal.source.repository_ids[0]==="repo-0");
    check("Draft with Manvi consumes notes after extract", idea.value.trim() === "");
    check("drafted sheet keeps a single Manvi assist section", Boolean(editor().querySelector('[aria-label="Manvi task assist"]')) && !editor().querySelector('[aria-label="Manvi task enhancements"]'));
    check("inline suggestions hide history field checkboxes until opened", [...editor().querySelectorAll('.history-drawer article input[type="checkbox"]')].length === 0 || editor().querySelector(".history-drawer")?.open === true);
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
    const footerBackground=getComputedStyle(editor().querySelector("footer")).backgroundColor;
    check("sticky save controls have an opaque background over scrolled fields", !footerBackground.startsWith("rgba") || footerBackground.endsWith(", 1)"));
    confirmAnswer=true; await click("Close task details");
    failConfiguration=true; await click("New task"); await wait(()=>editor().textContent.includes("Manvi temporarily unavailable"));
    check("inline configuration failure exposes a working retry", Boolean(button("Reload Manvi configuration")) && !button("Reload Manvi configuration").matches(":disabled"));
    failConfiguration=false; blankConfiguration=true; await click("Reload Manvi configuration");
    check("an unconfigured model exposes selection beside drafting", Boolean(editor().querySelector(".model-settings input")) && editor().querySelector(".model-settings").open && button("Improve with Manvi").matches(":disabled"));
    await change(editor().querySelector(".model-settings input"),"quick-fixture");
    await change(editor().querySelector(".notes-label textarea"),"A recoverable task");
    check("choosing a task model recovers drafting without reopening the editor", !button("Draft with Manvi").matches(":disabled"));
    editor().querySelectorAll(".field-picks button").forEach(el=>el.click()); await settle();
    const emptySelectionWrites=enhancementWrites.length;
    editor().querySelector(".notes-label textarea").dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",ctrlKey:true,bubbles:true})); await settle();
    check("empty field selection cannot widen through the keyboard shortcut", button("Draft with Manvi").matches(":disabled") && enhancementWrites.length===emptySelectionWrites);
    blankConfiguration=false; await click("Close task details");
    check("no runtime errors or unconfigured fixture requests occurred", crashes.length === 0 && unknown.length === 0);
  } catch(error) { results.push({name:error.message, stack:error.stack, pass:false}); }
  document.getElementById("verdict").textContent = JSON.stringify({results}, null, 2);
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({results})));
  if(params.has("report")) await fetch(params.get("report"), {method:"POST", body:document.documentElement.outerHTML});
}
