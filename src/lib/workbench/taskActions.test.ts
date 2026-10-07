import { afterEach, describe, expect, it, vi } from "vitest";
import { deleteTask, task, WorkbenchError, type Task } from "./client";
import { DEFERRED_DELETE_MS, TaskBatch, TASK_ACTION_TIMEOUT_MS, MAX_TASK_SELECTION, defer, describeTaskAction } from "./taskActions";
vi.mock("@tauri-apps/api/core", () => ({invoke: vi.fn()}));
import { invoke } from "@tauri-apps/api/core";
const sample = (id = "task"): Task => task({id,revision:2,updated_at:1,title:`Task ${id}`,description:"Preserve E42 exactly",acceptance_criteria:["Verify E42"],kind:"bug",status:"ready",priority:2,severity:null,owner:null,due_at:null,labels:["evidence"],repository_ids:["r1","r2"],primary_repository_id:"r1",home_workspace_id:null,position:10});
const io = () => ({read:vi.fn(async (id:string) => sample(id)), write:vi.fn(async (input:Record<string,unknown>) => ({...sample(String(input.id)),...input,revision:3})), remove:vi.fn(async (_input:Record<string,unknown>) => {})});
afterEach(() => { vi.useRealTimers(); vi.clearAllMocks(); });
describe("task deletion boundary", () => {
  it.each([{}, {ok:false}, {ok:true,item:{id:"task",revision:3}}, {ok:true,item:{id:"other",revision:3,deleted:true}}, {ok:true,item:{id:"task",revision:2,deleted:true}}, {ok:true,item:{id:"task",revision:3,deleted:"true"}}, {ok:true,item:null}])("rejects an unconfirmed or mismatched deletion: %j", async receipt => {
    vi.mocked(invoke).mockResolvedValueOnce(JSON.stringify(receipt));
    await expect(deleteTask({id:"task",expected_revision:2,request_id:"request"})).rejects.toMatchObject({code:"protocol_error"});
  });
  it("accepts only the matching tombstone revision", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(JSON.stringify({ok:true,item:{id:"task",revision:3,deleted:true}}));
    await expect(deleteTask({id:"task",expected_revision:2,request_id:"request"})).resolves.toBeUndefined();
  });
});
describe("bounded task operations", () => {
  it("rejects empty, duplicate and oversized selections", () => {
    for(const cards of [[],[sample(),sample()],Array.from({length:MAX_TASK_SELECTION+1},(_,i)=>sample(String(i)))]) expect(() => new TaskBatch(cards,{kind:"delete"},io())).toThrow();
  });
  it("preserves immutable selection revisions and request identities across lost replies", async () => {
    const port = io(), card = sample();
    port.remove.mockRejectedValueOnce(new WorkbenchError("transport_error","reply lost"));
    const batch = new TaskBatch([card],{kind:"delete"},port); card.revision = 99;
    await batch.run(); expect(batch.snapshot()[0].state).toBe("uncertain");
    await batch.run(); expect(batch.snapshot()[0].state).toBe("done");
    expect(port.remove.mock.calls[0][0]).toEqual(port.remove.mock.calls[1][0]);
    expect(port.remove.mock.calls[0][0].expected_revision).toBe(2);
  });
  it("pauses at uncertainty without issuing later deletes or replaying completed rows", async () => {
    const port = io(); port.remove.mockResolvedValueOnce().mockRejectedValueOnce(new WorkbenchError("protocol_error","bad receipt"));
    const batch = new TaskBatch([sample("a"),sample("b"),sample("c")],{kind:"delete"},port);
    await batch.run(); expect(batch.snapshot().map(row=>row.state)).toEqual(["done","uncertain","waiting"]);
    await batch.run(); expect(port.remove.mock.calls.map(([body])=>body.id)).toEqual(["a","b","b","c"]);
  });
  it("does not overwrite concurrently changed task records", async () => {
    const port = io(); port.read.mockResolvedValueOnce({...sample(),revision:3});
    const batch = new TaskBatch([sample()],{kind:"update",changes:{status:"done"}},port);
    await batch.run(); expect(batch.snapshot()[0].state).toBe("failed"); expect(port.write).not.toHaveBeenCalled();
  });
  it("preserves descriptions, criteria, links and other fields when organizing", async () => {
    const port = io(); const batch = new TaskBatch([sample()],{kind:"update",changes:{priority:0}},port);
    await batch.run(); expect(port.write.mock.calls[0][0]).toMatchObject({description:"Preserve E42 exactly",acceptance_criteria:["Verify E42"],repository_ids:["r1","r2"],priority:0});
  });
  it("retries the original update without rebuilding it from a later record", async () => {
    const port=io(); port.write.mockRejectedValueOnce(new WorkbenchError("transport_error","lost"));
    const batch=new TaskBatch([sample()],{kind:"update",changes:{status:"done"}},port);
    await batch.run(); await batch.run(); expect(port.read).toHaveBeenCalledTimes(1); expect(port.write.mock.calls[0][0]).toEqual(port.write.mock.calls[1][0]);
  });
  it("times out a hung operation and retains the exact retry", async () => {
    vi.useFakeTimers(); const port=io(); port.remove.mockImplementationOnce(() => new Promise(()=>{}));
    const batch=new TaskBatch([sample()],{kind:"delete"},port); const running=batch.run();
    await vi.advanceTimersByTimeAsync(TASK_ACTION_TIMEOUT_MS); await running;
    expect(batch.snapshot()[0].state).toBe("uncertain"); await batch.run();
    expect(port.remove.mock.calls[0][0]).toEqual(port.remove.mock.calls[1][0]);
  });
  it("serializes duplicate run calls and honors pause after the in-flight task", async () => {
    const port=io(); let release!:()=>void; port.remove.mockImplementationOnce(() => new Promise(resolve=>{release=resolve;}));
    const batch=new TaskBatch([sample("a"),sample("b")],{kind:"delete"},port);
    const first=batch.run(); await batch.run(); batch.stop(); release(); await first;
    expect(port.remove).toHaveBeenCalledTimes(1); expect(batch.snapshot().map(row=>row.state)).toEqual(["done","waiting"]);
    await batch.run(); expect(port.remove).toHaveBeenCalledTimes(2);
  });
  it("cancels before writing if a paused read completes", async () => {
    const port=io(); let release!:(value:Task)=>void; port.read.mockImplementationOnce(() => new Promise(resolve=>{release=resolve;}));
    const batch=new TaskBatch([sample()],{kind:"update",changes:{status:"done"}},port); const first=batch.run(); batch.stop(); release(sample()); await first;
    expect(port.write).not.toHaveBeenCalled(); expect(batch.snapshot()[0].state).toBe("waiting");
  });
  it("reports a mixed conflict batch exactly without force-deleting stale revisions", async () => {
    const port=io(); port.remove.mockRejectedValueOnce(new WorkbenchError("revision_conflict","changed"));
    const batch=new TaskBatch([sample("a"),sample("b")],{kind:"delete"},port); await batch.run(); await batch.run();
    expect(batch.snapshot().map(row=>row.state)).toEqual(["failed","done"]); expect(port.remove).toHaveBeenCalledTimes(2);
  });
  it("never has more than one deletion in flight under the selection cap", async () => {
    const port=io();let active=0,max=0;
    port.remove.mockImplementation(async ()=>{max=Math.max(max,++active);await Promise.resolve();active--;});
    const batch=new TaskBatch(Array.from({length:MAX_TASK_SELECTION},(_,i)=>sample(String(i))),{kind:"delete"},port); await batch.run();
    expect(max).toBe(1); expect(batch.snapshot().filter(row=>row.state==="done")).toHaveLength(MAX_TASK_SELECTION);
  });
});

describe("organization boundary hardening", () => {
  it.each([-1,4,NaN,Infinity])( "rejects invalid priority %s before a write", priority => {
    expect(()=>new TaskBatch([sample()],{kind:"update",changes:{priority}},io())).toThrow();
  });
  it("rejects unsafe integer positions and invalid revisions", () => {
    expect(()=>new TaskBatch([sample()],{kind:"update",changes:{position:Number.MAX_SAFE_INTEGER+1}},io())).toThrow();
    expect(()=>new TaskBatch([{...sample(),revision:0}],{kind:"delete"},io())).toThrow();
  });
  it("refuses a restore plan that names fields no board action writes, or omits a card", () => {
    const card = sample("a");
    expect(()=>new TaskBatch([card],{kind:"restore",fields:{a:{}}},io())).toThrow();
    expect(()=>new TaskBatch([card],{kind:"restore",fields:{a:{status:"done"},b:{status:"done"}}},io())).toThrow();
    expect(()=>new TaskBatch([card],{kind:"restore",fields:{}},io())).toThrow();
    const smuggled = {a:{status:"done" as const, description:"overwritten"}};
    expect(()=>new TaskBatch([card],{kind:"restore",fields:smuggled},io())).toThrow();
    expect(()=>new TaskBatch([card],{kind:"restore",fields:{a:{labels:["ok","bad\u0007"]}}},io())).toThrow();
    expect(()=>new TaskBatch([card],{kind:"restore",fields:{a:{priority:7}}},io())).toThrow();
  });
  it("re-spaces dense ordering while preserving each complete task", async () => {
    const port=io(); const batch=new TaskBatch([sample("a"),sample("b")],{kind:"reorder",status:"ready",positions:{a:1024,b:2048}},port);
    await batch.run(); expect(port.write.mock.calls.map(([body])=>body.position)).toEqual([1024,2048]);
    expect(port.write.mock.calls.every(([body])=>body.description === "Preserve E42 exactly")).toBe(true);
  });
});

/** A store that keeps what was written, so an undo reads what the batch left. */
function memoryStore(...seed: Task[]) {
  const rows = new Map(seed.map(item => [item.id, item]));
  return {
    rows,
    read: vi.fn(async (id: string) => { const row = rows.get(id); if (!row) throw new WorkbenchError("not_found","missing"); return row; }),
    write: vi.fn(async (input: Record<string, unknown>) => {
      const row = rows.get(String(input.id));
      if (!row || row.revision !== input.expected_revision) throw new WorkbenchError("revision_conflict","changed");
      const { expected_revision: _revision, request_id: _request, ...fields } = input;
      const saved = task({ ...row, ...fields, revision: row.revision + 1 });
      rows.set(saved.id, saved);
      return saved;
    }),
    remove: vi.fn(async (_input: Record<string, unknown>) => {}),
  };
}

describe("undo", () => {
  it("puts back exactly the fields a move wrote, per task, and leaves everything else", async () => {
    const store = memoryStore({...sample("a"),status:"ready",position:5}, {...sample("b"),status:"backlog",position:9});
    const move = new TaskBatch([store.rows.get("a")!, store.rows.get("b")!],{kind:"update",changes:{status:"review"}},store);
    await move.run();
    const plan = move.undo();
    expect(plan?.label).toBe("Moved 2 tasks to Review");
    expect(plan?.cards.map(card => card.revision)).toEqual([3,3]);
    await new TaskBatch(plan!.cards, plan!.action, store).run();
    expect(store.rows.get("a")).toMatchObject({status:"ready",position:5,revision:4,description:"Preserve E42 exactly"});
    expect(store.rows.get("b")).toMatchObject({status:"backlog",position:9,revision:4});
  });
  it("restores each task's own labels after a bulk label change", async () => {
    const store = memoryStore({...sample("a"),labels:["evidence"]}, {...sample("b"),labels:["ux","evidence"]});
    const tag = new TaskBatch([store.rows.get("a")!, store.rows.get("b")!],{kind:"label",label:"triaged",add:true},store);
    await tag.run();
    expect(store.rows.get("b")?.labels).toEqual(["ux","evidence","triaged"]);
    const plan = tag.undo()!;
    expect(plan.label).toBe("Added label “triaged” to 2 tasks");
    await new TaskBatch(plan.cards, plan.action, store).run();
    expect(store.rows.get("a")?.labels).toEqual(["evidence"]);
    expect(store.rows.get("b")?.labels).toEqual(["ux","evidence"]);
  });
  it("records what was saved, not what the board drew, as the value to restore", async () => {
    const store = memoryStore({...sample("a"),priority:1});
    const stale = {...store.rows.get("a")!, priority:3};
    const change = new TaskBatch([stale],{kind:"update",changes:{priority:0}},store);
    await change.run();
    await new TaskBatch(change.undo()!.cards, change.undo()!.action, store).run();
    expect(store.rows.get("a")?.priority).toBe(1);
  });
  it("refuses to undo a task someone edited after the change, and undoes the rest", async () => {
    const store = memoryStore(sample("a"), sample("b"));
    const move = new TaskBatch([store.rows.get("a")!, store.rows.get("b")!],{kind:"update",changes:{status:"done"}},store);
    await move.run();
    const plan = move.undo()!;
    const edited = store.rows.get("b")!;
    store.rows.set("b", {...edited, title:"Edited elsewhere", revision: edited.revision + 1});
    const undo = new TaskBatch(plan.cards, plan.action, store);
    await undo.run();
    expect(undo.snapshot().map(row => row.state)).toEqual(["done","failed"]);
    expect(store.rows.get("a")?.status).toBe("ready");
    expect(store.rows.get("b")).toMatchObject({status:"done",title:"Edited elsewhere"});
  });
  it("offers nothing to undo for a deletion, a restore, or a batch that changed nothing", async () => {
    const store = memoryStore(sample("a"));
    const removal = new TaskBatch([store.rows.get("a")!],{kind:"delete"},store);
    await removal.run();
    expect(removal.undo()).toBeNull();
    const failing = memoryStore(sample("a"));
    failing.write.mockRejectedValueOnce(new WorkbenchError("revision_conflict","changed"));
    const refused = new TaskBatch([failing.rows.get("a")!],{kind:"update",changes:{status:"done"}},failing);
    await refused.run();
    expect(refused.undo()).toBeNull();
    const done = memoryStore(sample("a"));
    const move = new TaskBatch([done.rows.get("a")!],{kind:"update",changes:{status:"done"}},done);
    await move.run();
    const back = new TaskBatch(move.undo()!.cards, move.undo()!.action, done);
    await back.run();
    expect(back.undo()).toBeNull();
  });
  it("undoes a reorder by restoring each card's column and position", async () => {
    const store = memoryStore({...sample("a"),status:"ready",position:1}, {...sample("b"),status:"backlog",position:2});
    const order = new TaskBatch([store.rows.get("a")!, store.rows.get("b")!],{kind:"reorder",status:"review",positions:{a:1024,b:2048}},store);
    await order.run();
    const plan = order.undo()!;
    expect(plan.label).toBe("Reordered 2 tasks in Review");
    await new TaskBatch(plan.cards, plan.action, store).run();
    expect(store.rows.get("a")).toMatchObject({status:"ready",position:1});
    expect(store.rows.get("b")).toMatchObject({status:"backlog",position:2});
  });
});

describe("describeTaskAction", () => {
  it("names what each kind of board action did", () => {
    expect(describeTaskAction({kind:"update",changes:{status:"done"}},1)).toBe("Moved 1 task to Done");
    expect(describeTaskAction({kind:"update",changes:{priority:0}},3)).toBe("Changed priority of 3 tasks");
    expect(describeTaskAction({kind:"update",changes:{owner:"ada"}},2)).toBe("Assigned 2 tasks to ada");
    expect(describeTaskAction({kind:"update",changes:{owner:null}},2)).toBe("Cleared the owner of 2 tasks");
    expect(describeTaskAction({kind:"update",changes:{due_at:null}},1)).toBe("Cleared the due date of 1 task");
    expect(describeTaskAction({kind:"label",label:"ux",add:false},2)).toBe("Removed label “ux” from 2 tasks");
    expect(describeTaskAction({kind:"delete"},2)).toBe("Deleted 2 tasks");
  });
});

describe("defer", () => {
  it("runs once when the window closes, and never after it was cancelled", async () => {
    vi.useFakeTimers();
    const run = vi.fn();
    defer(DEFERRED_DELETE_MS, run);
    await vi.advanceTimersByTimeAsync(DEFERRED_DELETE_MS - 1);
    expect(run).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(run).toHaveBeenCalledTimes(1);
    const cancelled = vi.fn();
    const pending = defer(DEFERRED_DELETE_MS, cancelled);
    expect(pending.cancel()).toBe(true);
    await vi.advanceTimersByTimeAsync(DEFERRED_DELETE_MS * 2);
    expect(cancelled).not.toHaveBeenCalled();
    expect(pending.flush()).toBe(false);
    expect(cancelled).not.toHaveBeenCalled();
  });
  it("runs at once on flush, and the timer does not run it again", async () => {
    vi.useFakeTimers();
    const run = vi.fn();
    const pending = defer(DEFERRED_DELETE_MS, run);
    expect(pending.flush()).toBe(true);
    expect(pending.cancel()).toBe(false);
    await vi.advanceTimersByTimeAsync(DEFERRED_DELETE_MS * 2);
    expect(run).toHaveBeenCalledTimes(1);
  });
});
