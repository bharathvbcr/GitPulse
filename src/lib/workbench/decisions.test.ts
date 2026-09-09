import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { agentDecision, attention, decisionQuestions, structuredDecisionAnswer, decisionWrite, listAgentDecisions, saveAgentDecision, type AgentDecision } from "./client";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const item: AgentDecision = { id:"decision",revision:1,updated_at:1000,run_id:"run",task_id:"task",source_revision:2,repository_id:"repo",repository_revision:3,
  owner_id:"host",session_id:"session",provider_thread_id:"thread",provider_turn_id:"turn",protocol_request_id:"n:42",provider:"codex",permission_mode:"ask",policy_revision:1,cwd:"/checkout",
  kind:"permission",payload:'{"command":"git status"}',payload_digest:"e0d3e391760d0a9b6c24bf66cecfc5a66557784782cbc704052385bf6e9bb287",created_at:1000,expires_at:1300,state:"pending",decision:null,answer:null,actionable:true,reason:"" };
beforeEach(() => native.mockReset());
describe("agent request review", () => {
  it("binds multiple answers to the captured questions and refuses secret or incomplete sets", () => {
    const question = {...item, kind:"question" as const, payload:JSON.stringify({method:"item/tool/requestUserInput",params:{questions:[{id:"scope",question:"Which scope?",options:[{label:"Focused",description:"Run focused tests"}]},{id:"output",question:"Which artifact?"}]}})};
    expect(decisionQuestions(question)).toMatchObject([{id:"scope",options:[{label:"Focused"}]},{id:"output"}]);
    expect(structuredDecisionAnswer(question,{scope:"Focused",output:"Report"})).toBe('{"scope":["Focused"],"output":["Report"]}');
    const invalidAnswers: Record<string, string>[] = [{scope:"Focused"},{scope:"Focused",output:" "},{scope:"Focused",output:"Report",foreign:"yes"}];
    for (const answers of invalidAnswers) expect(() => structuredDecisionAnswer(question,answers)).toThrow();
    expect(() => structuredDecisionAnswer(question,{scope:"🙂".repeat(4096),output:"Report"})).toThrow();
    expect(() => decisionQuestions({...question,payload:question.payload.replace('"id":"scope"','"id":"scope","isSecret":true')})).toThrow();
  });
  it("retains the full payload and immutable callback binding", () => { expect(agentDecision(item)).toEqual(item); });
  it.each([{payload:"[]"},{payload:"invalid"},{payload_digest:"a"},{policy_revision:0},{state:"approved"},{actionable:"true"},{expires_at:1301},{source_revision:0},{owner_id:"../host"},{state:"decided"},{decision:"allow_once"},{kind:"question",decision:"allow_once"}])("rejects an inconsistent review record %j", (change) => { expect(()=>agentDecision({...item,...change})).toThrow(); });
  it("recomputes the complete payload hash before saving a decision", async () => {
    const input=await decisionWrite(item,"allow_once");
    expect(input).toMatchObject({id:item.id,expected_revision:1,payload_digest:item.payload_digest,decision:"allow_once"});
    expect(input).not.toHaveProperty("skip_permissions");
    await expect(decisionWrite({...item,payload:'{"command":"git push"}'},"allow_once")).rejects.toMatchObject({code:"protocol_error"});
    await expect(decisionWrite({...item,actionable:false},"allow_once")).rejects.toMatchObject({code:"protocol_error"});
    expect(native).not.toHaveBeenCalled();
  });
  it("separates questions from tool permission",async()=>{
    const question={...item,kind:"question" as const};
    await expect(decisionWrite(question,"allow_once")).rejects.toThrow();
    await expect(decisionWrite(question,"answer"," ")).rejects.toThrow();
    expect(await decisionWrite(question,"answer","Run the focused checks")).toMatchObject({decision:"answer",answer:"Run the focused checks"});
  });
  it("retains one decision identity after a lost reply and rejects foreign receipts", async () => {
    const input=await decisionWrite(item,"allow_once");
    native.mockRejectedValueOnce({code:"transport_error",message:"lost reply"});
    await expect(saveAgentDecision(item,input)).rejects.toMatchObject({code:"transport_error"});
    const saved={...item,revision:2,state:"decided",decision:"allow_once",actionable:false};
    native.mockResolvedValueOnce(JSON.stringify({ok:true,item:saved,sequence:45}));
    expect(await saveAgentDecision(item,input)).toMatchObject({state:"decided",actionable:false});
    expect(native.mock.calls[0]).toEqual(native.mock.calls[1]);
    for(const changed of [{session_id:"other"},{provider_turn_id:"other"},{cwd:"/other"},{payload:'{"command":"git push"}'},{expires_at:1299}]) {
      native.mockResolvedValueOnce(JSON.stringify({ok:true,item:{...saved,...changed}}));
      await expect(saveAgentDecision(item,input)).rejects.toMatchObject({code:"protocol_error"});
    }
  });
  it("scopes bounded pages to the requested run",async()=>{
    const page={ok:true,items:[item],total:90,shown:1,has_more:true,next_cursor:"1:1000:decision"};
    native.mockResolvedValueOnce(JSON.stringify(page));
    expect(await listAgentDecisions("run")).toMatchObject({total:90,shown:1});
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request",{method:"decisions.list",input:'{"run_id":"run","limit":30}'});
    native.mockResolvedValueOnce(JSON.stringify(page));
    await expect(listAgentDecisions("other")).rejects.toMatchObject({code:"protocol_error"});
  });
  it("recognizes private permission and question notices without inventing resolved work",()=>{
    const notice={id:"event-45",revision:1,updated_at:1000,source_sequence:45,task_id:"task",task_revision:2,target_type:"decision",target_id:"decision",target_revision:1,target_status:"current",kind:"decision_permission",title:"Coding agent needs permission",created_at:1000,read_at:null,dismissed_at:null,snoozed_until:null};
    expect(attention(notice)).toMatchObject({target_type:"decision"});
    expect(attention({...notice,kind:"decision_question",title:"Coding agent needs input"})).toMatchObject({kind:"decision_question"});
  });
});
