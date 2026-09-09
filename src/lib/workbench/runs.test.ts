import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { cancelTaskRun, getRepository, getTaskRun, launchManagedRun, stopManagedRun, listTaskRuns, prepareTaskRun, taskRun, type RunPreparation, type TaskRun } from "./client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const run: TaskRun = {kind:"external_terminal",id:"run",revision:1,updated_at:100,task_id:"task",source_revision:2,task_title:"Preserve E42",repository_id:"repo",provider:"codex",permission_mode:"ask",state:"prepared",cwd:"/checkout with spaces",created_at:100,expires_at:400,session_id:null,exit_code:null,reason:"",outcome_uncertain:false};
const preparation: RunPreparation = {id:run.id,request_id:"prepare",task_id:run.task_id,source_revision:run.source_revision,repository_id:run.repository_id,repository_revision:3,repo_path:run.cwd,provider:run.provider,permission_mode:run.permission_mode,acknowledge_bypass:false};
beforeEach(() => native.mockReset());
describe("task attempt transport", () => {
  it("renders a managed initialization failure without inventing a provider identity", () => {
    const failed = {...run, kind:"managed", state:"exited", provider_state:"failed", reason:"Handshake rejected; process reaped"};
    expect(taskRun(failed)).toMatchObject(failed);
    expect(taskRun(failed).provider_thread_id).toBeUndefined();
    for (const provider_state of ["ready", "running", "completed"]) {
      expect(() => taskRun({...failed, provider_state})).toThrow();
    }
    for (const provider_state of ["running", "completed"]) {
      expect(() => taskRun({...failed, provider_state, provider_thread_id:"thread", provider_turn_id:null})).toThrow();
    }
  });
  it("routes managed launches through native identity observation and rejects terminal or foreign receipts", async () => {
    const managed = {...run, kind: "managed" as const, state: "running" as const, session_id: "session", provider_state: "running" as const, provider_thread_id:"thread", provider_turn_id:"turn"};
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:managed}));
    expect(await launchManagedRun("run")).toMatchObject(managed);
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {method:"runs.launch_managed",input:'{"id":"run"}'});
    for (const change of [{id:"other"},{kind:"external_terminal"},{provider:"claude"}]) {
      native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...managed,...change}}));
      await expect(launchManagedRun("run")).rejects.toMatchObject({code:"protocol_error"});
    }
    native.mockResolvedValueOnce('{"ok":true}');
    await stopManagedRun("run");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {method:"runs.stop_managed",input:'{"id":"run"}'});
    native.mockResolvedValueOnce('{"ok":false}');
    await expect(stopManagedRun("run")).rejects.toMatchObject({code:"protocol_error"});
  });
  it("preserves the exact preparation request and rejects mismatched native identity or policy", async () => {
    native.mockResolvedValue(JSON.stringify({ok:true,item:run}));
    expect(await prepareTaskRun(preparation)).toEqual(run);
    expect(native).toHaveBeenCalledWith("cmd_workbench_request",{method:"runs.prepare_terminal",input:JSON.stringify(preparation)});
    for (const change of [{id:"other"},{task_id:"other"},{source_revision:3},{repository_id:"other"},{provider:"claude"},{permission_mode:"bypass"},{state:"running"}]) {
      native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...run,...change}}));
      await expect(prepareTaskRun(preparation)).rejects.toMatchObject({code:"protocol_error"});
    }
  });
  it("refuses malformed states, revisions and process outcomes", () => {
    for (const change of [{state:"done"},{provider:"shell"},{permission_mode:"skip"},{source_revision:0},{exit_code:-1},{outcome_uncertain:"false"},{session_id:12},{reason:null}]) expect(()=>taskRun({...run,...change})).toThrow();
    expect(taskRun({...run,state:"unresolved",outcome_uncertain:true})).toMatchObject({state:"unresolved",outcome_uncertain:true});
  });
  it("checks repository and task-run identity in single-record reads", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{id:"repo",revision:3,updated_at:1,name:"GitPulse",identity_key:"local",remote_url:null}}));
    await expect(getRepository("other")).rejects.toMatchObject({code:"protocol_error"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:run}));
    await expect(getTaskRun("other")).rejects.toMatchObject({code:"protocol_error"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:run}));
    expect(await getTaskRun(run.id)).toEqual(run);
  });
  it("loads bounded newest-first history and checks every task identity", async () => {
    const page={ok:true,items:[run],shown:1,total:41,has_more:true,next_cursor:"1:99:older"};
    native.mockResolvedValueOnce(JSON.stringify(page));
    expect(await listTaskRuns("task","1:100:run")).toMatchObject({total:41,shown:1});
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request",{method:"runs.list",input:'{"task_id":"task","limit":30,"newest":true,"cursor":"1:100:run"}'});
    native.mockResolvedValueOnce(JSON.stringify(page));
    await expect(listTaskRuns("other")).rejects.toMatchObject({code:"protocol_error"});
  });
  it("cancels only the requested preparation and retains provider errors", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...run,revision:2,state:"cancelled"}}));
    expect(await cancelTaskRun(run)).toMatchObject({id:"run",state:"cancelled"});
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...run,id:"other",revision:2,state:"cancelled"}}));
    await expect(cancelTaskRun(run)).rejects.toMatchObject({code:"protocol_error"});
    native.mockRejectedValueOnce({code:"repository_changed",message:"Checkout moved"});
    await expect(prepareTaskRun(preparation)).rejects.toMatchObject({code:"repository_changed"});
  });
});
