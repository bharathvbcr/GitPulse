/**
 * The page's side of `src-tauri/src/mavi/mod.rs`. Every Mavi command crosses
 * IPC here and nowhere else.
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import type {
  MaviConversationCheck,
  MaviEntry,
  MaviEventPayload,
  MaviFileDiff,
  MaviProbe,
  MaviReplay,
  MaviSummary,
} from "./types";

export const MAVI_EVENT = "mavi-event";
export const MAVI_STATUS_EVENT = "mavi-status";

/** Least authority first, as the shared policy table orders them. */
export const MAVI_MODES = [
  { id: "inspect", label: "Plan only", hint: "Reads and plans. Never edits or runs commands." },
  { id: "ask", label: "Ask first", hint: "Asks before edits and commands your settings do not already allow." },
  { id: "edit", label: "Edit files", hint: "Edits files without asking. Still asks before other commands." },
  { id: "auto_review", label: "Auto", hint: "The agent's own classifier decides what needs asking." },
  { id: "preapproved", label: "Preapproved only", hint: "Runs only what your settings preapprove; refuses the rest." },
  { id: "bypass", label: "Bypass", hint: "Runs everything without asking. Needs acknowledgement." },
] as const;

export type MaviModeId = (typeof MAVI_MODES)[number]["id"];

export interface StartRequest {
  repoPath?: string;
  host?: string;
  dir?: string;
  mode: MaviModeId;
  acknowledged?: boolean;
  model?: string;
  resume?: string;
  /** The page's `ThreadRecord.key`: names the thread that owns terminal tabs its agent starts. */
  threadKey?: string;
}

export type Probe = MaviProbe;

export const maviClient = {
  start: (request: StartRequest) => invoke<MaviSummary>("cmd_mavi_start", { request }),
  send: (id: string, text: string) => invoke<number>("cmd_mavi_send", { id, text }),
  answer: (id: string, requestId: string, decision: "allow" | "deny", message?: string) =>
    invoke<void>("cmd_mavi_answer", { id, requestId, decision, message: message ?? null }),
  interrupt: (id: string) => invoke<void>("cmd_mavi_interrupt", { id }),
  stop: (id: string) => invoke<void>("cmd_mavi_stop", { id }),
  list: () => invoke<MaviSummary[]>("cmd_mavi_list"),
  events: (id: string, after: number) => invoke<MaviReplay>("cmd_mavi_events", { id, after }),
  fileDiff: (id: string, path: string) => invoke<MaviFileDiff>("cmd_mavi_file_diff", { id, path }),
  sshHosts: () => invoke<string[]>("cmd_mavi_ssh_hosts"),
  sshProbe: (host: string, dir: string) => invoke<MaviProbe>("cmd_mavi_ssh_probe", { host, dir }),
  conversationCheck: (repoPath: string, conversationId: string) =>
    invoke<MaviConversationCheck>("cmd_mavi_conversation_check", { repoPath, conversationId }),
};

export function onMaviEvents(handler: (id: string, entries: MaviEntry[]) => void): Promise<UnlistenFn> {
  return listen<MaviEventPayload>(MAVI_EVENT, (event) => handler(event.payload.id, event.payload.entries));
}

export function onMaviStatus(handler: (summary: MaviSummary) => void): Promise<UnlistenFn> {
  return listen<MaviSummary>(MAVI_STATUS_EVENT, (event) => handler(event.payload));
}
