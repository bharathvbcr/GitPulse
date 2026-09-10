import type { TaskCard } from "./client";

export type TaskSort = "manual" | "priority" | "recent" | "title";
export type TaskFilter = "all" | "unassigned" | "high" | "overdue";
export function organizeTasks(tasks: readonly TaskCard[], sort: TaskSort, filter: TaskFilter, label: string, kind: string, now = Date.now() / 1000): TaskCard[] {
  const needle = label.trim().toLocaleLowerCase();
  return tasks.filter(task => (!kind || task.kind === kind) && (!needle || task.labels.some(value => value.toLocaleLowerCase().includes(needle))) &&
    (filter === "all" || filter === "unassigned" && !task.owner?.trim() || filter === "high" && task.priority <= 1 || filter === "overdue" && task.due_at !== null && task.due_at < now && task.status !== "done"))
    .sort((a,b) => (sort === "priority" ? a.priority - b.priority : sort === "recent" ? b.updated_at - a.updated_at : sort === "title" ? a.title.localeCompare(b.title) : a.position - b.position) || a.id.localeCompare(b.id));
}
export function selectedRange(ids: readonly string[], selected: readonly string[], target: string, anchor: string | null, extend: boolean, limit: number): string[] {
  const allowed = new Set(ids), retained = selected.filter(id => allowed.has(id));
  const end = ids.indexOf(target), start = anchor ? ids.indexOf(anchor) : -1;
  if (end < 0) return retained;
  if (extend && start >= 0) return [...new Set([...retained,...ids.slice(Math.min(start,end),Math.max(start,end)+1)])].slice(0,limit);
  return retained.includes(target) ? retained.filter(id => id !== target) : [...retained,target].slice(0,limit);
}

/** Local datetime input values preserve the user's timezone when round-tripped. */
export function localTaskDate(seconds: number | null): string {
  if (seconds === null) return "";
  const date = new Date(seconds * 1000), pad = (n:number) => String(n).padStart(2,"0");
  if (!Number.isFinite(date.getTime())) return "";
  return `${date.getFullYear()}-${pad(date.getMonth()+1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** Re-space a fully loaded column when its integer positions have no insertion gap. */
export function reorderPlan(items: readonly TaskCard[], dragged: TaskCard, index: number): {cards:TaskCard[]; positions:Record<string,number>} {
  const cards = items.filter(card=>card.id !== dragged.id);
  cards.splice(Math.max(0,Math.min(cards.length,index)),0,dragged);
  return {cards,positions:Object.fromEntries(cards.map((card,i)=>[card.id,(i+1)*1_048_576]))};
}
