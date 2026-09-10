import { describe, expect, it } from "vitest";
import { organizeTasks, selectedRange, localTaskDate } from "./taskOrganization";
import type { TaskCard } from "./client";
const card = (id:string, overrides:Partial<TaskCard> = {}):TaskCard => ({id,revision:1,updated_at:1,title:id,kind:"bug",status:"ready",priority:2,owner:null,severity:null,due_at:null,labels:[],repository_ids:["repo"],primary_repository_id:"repo",home_workspace_id:null,position:1,...overrides});
describe("task organization", () => {
  it("combines exact kind, label, priority and assignment filters without altering inputs", () => {
    const tasks=[card("a",{labels:["UI"],priority:0}),card("b",{kind:"feature",labels:["ui"],priority:1}),card("c",{labels:["UI"],owner:"Ada"})];
    expect(organizeTasks(tasks,"manual","high","ui","bug").map(c=>c.id)).toEqual(["a"]);
    expect(organizeTasks(tasks,"manual","unassigned","","bug").map(c=>c.id)).toEqual(["a"]);
    expect(tasks.map(c=>c.id)).toEqual(["a","b","c"]);
  });
  it("treats a due date at the current instant and completed tasks as not overdue", () => {
    const tasks=[card("a",{due_at:99}),card("b",{due_at:100}),card("c",{due_at:1,status:"done"}),card("d")];
    expect(organizeTasks(tasks,"manual","overdue","","",100).map(c=>c.id)).toEqual(["a"]);
  });
  it.each(["manual","priority","recent","title"] as const)("sorts %s with stable identity tie breaks", sort => {
    const tasks=[card("b"),card("a")]; expect(organizeTasks(tasks,sort,"all","","").map(c=>c.id)).toEqual(["a","b"]);
  });
  it("range selection is bounded and excludes hidden or unloaded IDs", () => {
    expect(selectedRange(["a","b","c","d"],["hidden"],"d","a",true,3)).toEqual(["a","b","c"]);
    expect(selectedRange(["a","b"],["a"],"a",null,false,100)).toEqual([]);
    expect(selectedRange(["a","b"],["a","hidden"],"missing",null,false,100)).toEqual(["a"]);
  });
  it("round-trips a date without shifting local wall time", () => {
    const stamp=new Date(2026,8,9,14,30).getTime()/1000;
    expect(localTaskDate(stamp)).toBe("2026-09-09T14:30"); expect(new Date(localTaskDate(stamp)).getTime()/1000).toBe(stamp);
    expect(localTaskDate(null)).toBe(""); expect(localTaskDate(Infinity)).toBe("");
  });
  it("stress-tests deterministic sorting and capped selection over 10,000 tasks", () => {
    const tasks=Array.from({length:10_000},(_,i)=>card(String(i),{priority:i%4,position:10_000-i}));
    for(const sort of ["manual","priority","recent","title"] as const) {
      const sorted=organizeTasks(tasks,sort,"all","","");
      expect(new Set(sorted.map(task=>task.id)).size).toBe(10_000);
      expect(selectedRange(sorted.map(task=>task.id),[],sorted.at(-1)!.id,sorted[0].id,true,100)).toHaveLength(100);
    }
  });
});

describe("dense manual ordering", () => {
  it("creates unique integer positions in the requested order, even for tied ranks", async () => {
    const {reorderPlan}=await import("./taskOrganization");
    const {cards,positions}=reorderPlan([card("a",{position:10}),card("c",{position:11})],card("b",{position:10}),1);
    expect(cards.map(c=>c.id)).toEqual(["a","b","c"]);
    expect(positions.a).toBeLessThan(positions.b); expect(positions.b).toBeLessThan(positions.c);
    expect(Object.values(positions).every(Number.isSafeInteger)).toBe(true);
  });
});
