import { beforeEach, describe, expect, it, vi } from "vitest";
const native=vi.hoisted(()=>vi.fn());
vi.mock("@tauri-apps/api/core",()=>({invoke:native}));
import { acknowledgeNotification, nativeNotificationStatus, newID, notificationDelivery, notificationDraft, notificationSettings, pendingNotificationActivation, putNotificationSettings } from "./client";
const settings={id:"profile",revision:1,updated_at:0,profile_id:"0123456789abcdef0123456789abcdef",enabled:false,sound:false,background:false,after_sequence:0,quiet_start:null,quiet_end:null,muted_workspace_ids:[],muted_repository_ids:[],muted_task_ids:[]};
const delivery={id:"event-42",revision:2,updated_at:1001,created_at:1000,native_id:`gitpulse.${settings.profile_id}.event-42`,task_id:"task",state:"submitted",activated_at:1001,acknowledged_at:null};
beforeEach(()=>native.mockReset());
describe("native notification boundaries",()=>{
  it("keeps mute scopes and quiet hours while excluding store-owned identity from edits",()=>{
    const parsed=notificationSettings({...settings,quiet_start:1320,quiet_end:420,muted_repository_ids:["repo"]});
    expect(notificationDraft(parsed)).toEqual({...notificationDraft(settings),quiet_start:1320,quiet_end:420,muted_repository_ids:["repo"]});
    expect(notificationDraft(parsed)).not.toHaveProperty("profile_id");
    expect(notificationDraft(parsed)).not.toHaveProperty("after_sequence");
  });
  it.each([{profile_id:"other"},{id:"other"},{quiet_start:0},{quiet_start:60,quiet_end:60},{quiet_start:1440,quiet_end:60},{enabled:1},{muted_repository_ids:["r","r"]},{muted_task_ids:["../../other"]},{muted_workspace_ids:Array.from({length:65},(_,i)=>`w${i}`)}])("rejects invalid settings %j",change=>{expect(()=>notificationSettings({...settings,...change})).toThrow();});
  it("retries the exact settings write after a lost reply and validates the revision",async()=>{
    const input={...notificationDraft(settings),id:"profile" as const,expected_revision:1,request_id:newID(),enabled:true};
    native.mockRejectedValueOnce({code:"transport_error",message:"Reply lost"});
    await expect(putNotificationSettings(input)).rejects.toMatchObject({code:"transport_error"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,sequence:9,item:{...settings,enabled:true,revision:2,updated_at:1000}}));
    expect((await putNotificationSettings(input)).enabled).toBe(true);
    expect(native.mock.calls[0]).toEqual(native.mock.calls[1]);
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:settings}));
    await expect(putNotificationSettings(input)).rejects.toMatchObject({code:"protocol_error"});
  });
  it("requests OS authorization only through the explicit authorize operation",async()=>{
    native.mockResolvedValue(JSON.stringify({available:true,authorization:"authorized",error:null}));
    await nativeNotificationStatus();
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request",{method:"notifications.native.status",input:"{}"});
    await nativeNotificationStatus(true);
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request",{method:"notifications.native.authorize",input:"{}"});
    native.mockResolvedValueOnce(JSON.stringify({available:false,authorization:"authorized",error:null}));
    await expect(nativeNotificationStatus()).rejects.toMatchObject({code:"protocol_error"});
  });
  it.each([{native_id:"gitpulse.other.event-42"},{id:"event-43"},{state:"delivered"},{state:"approved"},{task_id:"../r"},{activated_at:null,acknowledged_at:1}])("rejects forged or misleading delivery metadata %j",change=>{expect(()=>notificationDelivery({...delivery,...change})).toThrow();});
  it("requires a pending durable activation instead of accepting an arbitrary event payload",async()=>{
    native.mockResolvedValueOnce(JSON.stringify({ok:true,items:[delivery],shown:1,total:2,has_more:true,next_cursor:"1:1001:event-42"}));
    expect(await pendingNotificationActivation()).toEqual(delivery);
    native.mockResolvedValueOnce(JSON.stringify({ok:true,items:[{...delivery,activated_at:null}],shown:1,total:1,has_more:false,next_cursor:null}));
    await expect(pendingNotificationActivation()).rejects.toMatchObject({code:"protocol_error"});
  });
  it("recovers a lost acknowledgment without mutating the task or sending another receipt",async()=>{
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:delivery}));
    native.mockRejectedValueOnce({code:"transport_error",message:"Reply lost"});
    await expect(acknowledgeNotification(delivery.id)).rejects.toMatchObject({code:"transport_error"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...delivery,revision:3,acknowledged_at:1002}}));
    await acknowledgeNotification(delivery.id);
    expect(native.mock.calls.filter(call=>call[1].method==="notifications.ack")).toHaveLength(1);
    expect(native.mock.calls.every(call=>call[1].method.startsWith("notifications."))).toBe(true);
  });
});
