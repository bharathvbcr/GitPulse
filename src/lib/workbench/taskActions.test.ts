import { afterEach, describe, expect, it, vi } from "vitest";
import { deleteTask, task, WorkbenchError, type Task } from "./client";
import { TaskBatch, TASK_ACTION_TIMEOUT_MS, MAX_TASK_SELECTION } from "./taskActions";
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
  it("re-spaces dense ordering while preserving each complete task", async () => {
    const port=io(); const batch=new TaskBatch([sample("a"),sample("b")],{kind:"reorder",status:"ready",positions:{a:1024,b:2048}},port);
    await batch.run(); expect(port.write.mock.calls.map(([body])=>body.position)).toEqual([1024,2048]);
    expect(port.write.mock.calls.every(([body])=>body.description === "Preserve E42 exactly")).toBe(true);
  });
});
