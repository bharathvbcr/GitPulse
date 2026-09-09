import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { attention, attentionWrite, getAttention, listAttention, updateAttention, type Attention } from "./client";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const item: Attention = { id:"event-42",revision:1,updated_at:1000,source_sequence:42,task_id:"task",task_revision:3,target_type:"run",target_id:"run",target_revision:4,target_status:"current",kind:"run_exited",title:"Coding agent exited",created_at:1000,read_at:null,dismissed_at:null,snoozed_until:null };
beforeEach(() => native.mockReset());
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
