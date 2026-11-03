import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { attention, attentionRow, attentionWrite, getAttention, listAttention, updateAttention, UNREADABLE_TITLE, type Attention } from "./client";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const item: Attention = { id:"event-42",revision:1,updated_at:1000,source_sequence:42,task_id:"task",task_revision:3,target_type:"run",target_id:"run",target_revision:4,target_status:"current",kind:"run_exited",title:"Coding agent exited",created_at:1000,read_at:null,dismissed_at:null,snoozed_until:null };
const signal = { source:"github",external_id:"77",kind:"ci_failed",conclusion:"failure",workflow:"CI",branch:"gitpulse/fix-1234abcd",url:"https://github.com/acme/app/actions/runs/77",head_sha:"b".repeat(40) } as const;
const ciFailed = { ...item, id:"event-43",source_sequence:43,kind:"run_ci_failed",title:"Coding agent branch failed CI",signal } as const;
beforeEach(() => native.mockReset());
describe("CI notices from a host watcher", () => {
  it("decodes each CI kind with the exact title the store writes and the facts it carries", () => {
    for (const [kind, title, kindOfSignal] of [["run_ci_passed","Coding agent branch passed CI","ci_passed"],["run_ci_failed","Coding agent branch failed CI","ci_failed"],["run_ci_cancelled","Coding agent branch CI was cancelled","ci_cancelled"]] as const) {
      const notice = attention({ ...ciFailed, kind, title, signal: { ...signal, kind: kindOfSignal } });
      expect(notice.kind).toBe(kind);
      expect(notice.title).toBe(title);
      expect(notice.signal).toEqual({ ...signal, kind: kindOfSignal });
      expect(notice.target_type).toBe("run");
    }
  });
  it("keeps the signal's absent url as null and reads a local result", () => {
    const local = attention({ ...ciFailed, signal: { ...signal, source: "ci_local", url: null, workflow: "ci.local" } });
    expect(local.signal).toMatchObject({ source: "ci_local", url: null, workflow: "ci.local" });
  });
  it("refuses a CI notice without its facts, and facts on a notice that has none", () => {
    const { signal: _omitted, ...bare } = ciFailed;
    expect(() => attention(bare)).toThrow();
    expect(() => attention({ ...item, signal })).toThrow();
  });
  it.each([{source:"jenkins"},{kind:"ci_passed"},{external_id:"../77"},{conclusion:"FAILURE"},{workflow:" "},{workflow:"x".repeat(201)},{branch:""},{branch:"a\nb"},{url:"http://insecure.example/run"},{url:"https://x y"},{head_sha:"B".repeat(40)},{head_sha:"abc"}])("refuses a malformed signal %j", (change) => {
    expect(() => attention({ ...ciFailed, signal: { ...signal, ...change } })).toThrow();
  });
});
describe("a kind this version does not know", () => {
  const newer = { ...item, id:"event-44",source_sequence:44,kind:"run_quarantined",title:"Coding agent was quarantined",target_type:"run" };
  it("is an explicit unreadable row, not an error", () => {
    const row = attentionRow(newer);
    expect(row).toMatchObject({ unreadable: true, id: "event-44", kind: "run_quarantined", title: UNREADABLE_TITLE, read_at: null });
    expect("task_id" in row).toBe(false);
  });
  it("does not take the rest of the page with it", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ok:true,items:[item,newer,ciFailed],total:3,shown:3,has_more:false,next_cursor:null}));
    const loaded = await listAttention({kind:"global"},"all");
    expect(loaded.items.map((row) => "unreadable" in row ? "unreadable" : row.kind)).toEqual(["run_exited","unreadable","run_ci_failed"]);
    expect(loaded.total).toBe(3);
  });
  it("still refuses the single-notice reads, which act on a notice and so must understand it", async () => {
    expect(() => attention(newer)).toThrow();
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:newer}));
    await expect(getAttention("event-44")).rejects.toMatchObject({code:"protocol_error"});
  });
  it.each([{kind:"Run Quarantined"},{kind:"run-quarantined"},{kind:"x".repeat(65)},{kind:7},{id:"event-45"},{revision:0},{created_at:-1},{read_at:"never"}])("is still corruption when the rest is malformed %j", (change) => {
    expect(() => attentionRow({ ...newer, ...change })).toThrow();
  });
  it("leaves a malformed known kind to refuse the page", () => {
    expect(() => attentionRow({ ...item, title: "Accept this change" })).toThrow();
  });
});
describe("durable attention inbox", () => {
  it("retains valid notice identity and explicit target freshness", () => {
    expect(attention(item)).toEqual(item);
    for (const target_status of ["current","changed","task_changed","task_deleted","unavailable"] as const) expect(attention({...item,target_status}).target_status).toBe(target_status);
  });
  it.each([{source_sequence:41},{revision:0},{task_revision:0},{target_revision:0},{target_type:"permission"},{target_type:"enhancement"},{target_status:"approved"},{kind:"run_done"},{title:"Accept this change"},{task_id:"../repo"},{target_id:""},{read_at:-1},{dismissed_at:false},{snoozed_until:"never"}])("refuses malformed or mismatched notice metadata %j", (change) => { expect(()=>attention({...item,...change})).toThrow(); });
  it("pages the selected scope with truthful counts and a bounded request", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ok:true,items:[item],total:60,shown:1,has_more:true,next_cursor:"1:42:event-42"}));
    expect(await listAttention({kind:"workspace",id:"group"},"unread","1:80:event-80")).toMatchObject({shown:1,total:60});
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request",{method:"attention.list",input:'{"workspace_id":"group","filter":"unread","limit":30,"cursor":"1:80:event-80"}'});
  });
  it("checks identity again before opening a notice", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item}));
    expect(await getAttention(item.id)).toEqual(item);
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item}));
    await expect(getAttention("event-44")).rejects.toMatchObject({code:"protocol_error"});
  });
  it("retains the saved idempotency key after an uncertain action and never sends provider authority", async () => {
    const input=attentionWrite(item,"snooze");
    expect(input).toMatchObject({id:item.id,expected_revision:1,action:"snooze",seconds:3600});
    native.mockRejectedValueOnce({code:"transport_error",message:"Reply lost"});
    await expect(updateAttention(input)).rejects.toMatchObject({code:"transport_error"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,sequence:43,item:{...item,revision:2,snoozed_until:4600}}));
    await updateAttention(input);
    expect(native.mock.calls[0]).toEqual(native.mock.calls[1]);
    expect(JSON.stringify(input)).not.toContain("permission");
  });
  it("refuses action receipts from another notice or revision", async () => {
    const input=attentionWrite(item,"read");
    for (const changed of [item,{...item,id:"event-43",source_sequence:43,revision:2}]) {
      native.mockResolvedValueOnce(JSON.stringify({ok:true,item:changed}));
      await expect(updateAttention(input)).rejects.toMatchObject({code:"protocol_error"});
    }
  });
});
