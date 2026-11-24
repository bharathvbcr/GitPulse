/**
 * Timeline items → display rows. Consecutive tool calls collapse into one
 * group row with a verb summary ("Read 3 files · Edited 2 files"), so a turn
 * reads as what the agent did rather than a wall of calls. Three rules keep
 * the collapse honest:
 * - a call waiting for permission is never folded: it is its own row;
 * - a failed or denied call marks the group, and the mark survives collapse;
 * - edits are never invisible: the group carries the files it edited.
 */
import type { ThinkingItem, TimelineItem, ToolItem } from "./types";

export type Row =
  | { readonly kind: "item"; readonly key: string; readonly item: Exclude<TimelineItem, ToolItem> }
  | { readonly kind: "approval"; readonly key: string; readonly tool: ToolItem }
  | {
      readonly kind: "tools";
      readonly key: string;
      readonly tools: readonly ToolItem[];
      readonly summary: string;
      readonly failed: number;
      readonly running: ToolItem | null;
      readonly editedFiles: readonly string[];
    };

export type ToolVerb = "read" | "edit" | "command" | "search" | "web" | "agent" | "plan" | "ask" | "other";

const EDIT_TOOLS = new Set(["Edit", "MultiEdit", "Write", "NotebookEdit"]);

export function verbOf(name: string): ToolVerb {
  if (name === "Read" || name === "NotebookRead") return "read";
  if (EDIT_TOOLS.has(name)) return "edit";
  if (name === "Bash" || name === "BashOutput" || name === "KillShell" || name === "KillBash") return "command";
  if (name === "Grep" || name === "Glob" || name === "LS") return "search";
  if (name === "WebFetch" || name === "WebSearch") return "web";
  if (name === "Task" || name === "Agent") return "agent";
  if (name === "TodoWrite" || name === "ExitPlanMode" || name === "EnterPlanMode") return "plan";
  if (name === "AskUserQuestion") return "ask";
  return "other";
}

export function isEditTool(name: string): boolean {
  return EDIT_TOOLS.has(name);
}

/** The file a tool call is about, when it names one. */
export function toolPath(tool: ToolItem): string | null {
  const input = tool.input;
  const candidate = input.file_path ?? input.notebook_path ?? input.path;
  return typeof candidate === "string" && candidate ? candidate : null;
}

/**
 * `path` relative to `root` with `/` separators, or null when it lies outside.
 * Claude Code reports Windows paths with `\` and git with `/`, so both are
 * compared as one; a drive-letter or UNC root is compared without case, as
 * Windows does.
 */
export function relativeTo(path: string, root: string): string | null {
  const norm = (p: string) => p.replace(/\\/g, "/");
  const base = norm(root).replace(/\/+$/, "");
  const full = norm(path);
  if (!base || full.length <= base.length + 1 || full[base.length] !== "/") return null;
  const windows = /^[A-Za-z]:\//.test(base) || base.startsWith("//");
  const head = full.slice(0, base.length);
  if (windows ? head.toLowerCase() !== base.toLowerCase() : head !== base) return null;
  return full.slice(base.length + 1);
}

export function shortPath(path: string, root: string | null): string {
  return (root && relativeTo(path, root)) || path;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** One phrase per verb, in the order the agent first did each. */
export function summarize(tools: readonly ToolItem[]): string {
  const order: ToolVerb[] = [];
  const counts = new Map<ToolVerb, number>();
  const files = new Map<ToolVerb, Set<string>>();
  const others = new Set<string>();
  for (const tool of tools) {
    const verb = verbOf(tool.name);
    if (!counts.has(verb)) order.push(verb);
    counts.set(verb, (counts.get(verb) ?? 0) + 1);
    const path = toolPath(tool);
    if (path) {
      const set = files.get(verb) ?? new Set();
      set.add(path);
      files.set(verb, set);
    }
    if (verb === "other") others.add(tool.name.replace(/^mcp__[^_]+__/, ""));
  }
  return order
    .map((verb) => {
      const n = counts.get(verb)!;
      const f = files.get(verb)?.size ?? n;
      switch (verb) {
        case "read":
          return `Read ${plural(f, "file", "files")}`;
        case "edit":
          return `Edited ${plural(f, "file", "files")}`;
        case "command":
          return `Ran ${plural(n, "command", "commands")}`;
        case "search":
          return `Searched ${n === 1 ? "once" : `${n} times`}`;
        case "web":
          return `Looked up ${plural(n, "page", "pages")}`;
        case "agent":
          return `Ran ${plural(n, "subagent", "subagents")}`;
        case "plan":
          return "Updated the plan";
        case "ask":
          return "Asked a question";
        default:
          return `Used ${[...others].slice(0, 3).join(", ")}`;
      }
    })
    .join(" · ");
}

/** A one-line description of one call, for the expanded group. */
export function describeTool(tool: ToolItem, root: string | null): string {
  const input = tool.input;
  const path = toolPath(tool);
  const shown = path ? shortPath(path, root) : null;
  switch (tool.name) {
    case "Bash":
      return typeof input.command === "string" ? input.command : "Command";
    case "Grep":
      return `${typeof input.pattern === "string" ? input.pattern : ""}${typeof input.path === "string" ? ` in ${shortPath(input.path, root)}` : ""}`;
    case "Glob":
      return typeof input.pattern === "string" ? input.pattern : "Glob";
    case "WebFetch":
      return typeof input.url === "string" ? input.url : "Fetch";
    case "WebSearch":
      return typeof input.query === "string" ? input.query : "Search";
    case "Task":
    case "Agent":
      return typeof input.description === "string" ? input.description : "Subagent";
    default:
      if (shown) return shown;
      if (tool.partialInput) {
        const match = /"(?:file_path|command|pattern)"\s*:\s*"([^"]*)/.exec(tool.partialInput);
        if (match) return shortPath(match[1]!, root);
      }
      return tool.name;
  }
}

/** Items → rows. Thinking with no visible text folds into its neighbours' flow as its own small row. */
function lastRunning(tools: readonly ToolItem[]): ToolItem | null {
  for (let i = tools.length - 1; i >= 0; i--) {
    const tool = tools[i]!;
    if (tool.status === "running" && !tool.background) return tool;
  }
  return null;
}

export function toRows(items: readonly TimelineItem[], root: string | null): Row[] {
  const rows: Row[] = [];
  let group: ToolItem[] = [];
  // A group's key names the message row before it and its place after that
  // row, not its first tool: an answered request that joins the group must
  // not rename it (that would collapse a group the reader had opened).
  let anchor = "start";
  let nth = 0;
  const close = () => {
    if (!group.length) return;
    const tools = group;
    const edited: string[] = [];
    for (const tool of tools) {
      const path = toolPath(tool);
      if (path && isEditTool(tool.name)) {
        const short = shortPath(path, root);
        if (!edited.includes(short)) edited.push(short);
      }
    }
    rows.push({
      kind: "tools",
      key: `g:${anchor}:${nth++}`,
      tools,
      summary: summarize(tools),
      failed: tools.filter((t) => t.status === "error" || t.status === "denied").length,
      running: lastRunning(tools),
      editedFiles: edited,
    });
    group = [];
  };
  for (const item of items) {
    if (item.kind === "tool") {
      // An open request is always shown, a subagent's too: hiding it would
      // leave the agent waiting on an answer nobody can give.
      if (item.approval?.state === "open") {
        close();
        rows.push({ kind: "approval", key: `a:${item.id}`, tool: item });
        continue;
      }
      // Other subagent calls belong to their Task row, not the main flow.
      if (item.parentId) continue;
      group.push(item);
      continue;
    }
    if (item.kind === "thinking" && !item.text.trim() && !item.streaming) {
      // A finished thought with nothing to show: keep the group together.
      continue;
    }
    close();
    rows.push({ kind: "item", key: item.id, item: item as Exclude<TimelineItem, ToolItem> });
    anchor = item.id;
    nth = 0;
  }
  close();
  return rows;
}

/** Whether a thinking item is the live "Thinking…" row. */
export function isLiveThinking(item: ThinkingItem): boolean {
  return item.streaming;
}
