import { get, writable } from "svelte/store";

export interface InboxFocusRequest {
  threadKey: string;
  repoPath: string | null;
  seq: number;
}

const pending = writable<InboxFocusRequest | null>(null);
let nextSeq = 0;

export const inboxFocusRequest = { subscribe: pending.subscribe };

export function requestInboxFocus(threadKey: string, repoPath: string | null = null): void {
  const trimmed = threadKey.trim();
  if (!trimmed) return;
  const cleanRepo = typeof repoPath === "string" ? repoPath.trim() : "";
  nextSeq += 1;
  pending.set({
    threadKey: trimmed,
    repoPath: cleanRepo || null,
    seq: nextSeq,
  });
}

export function takeInboxFocus(): InboxFocusRequest | null {
  const current = get(pending);
  if (current) pending.set(null);
  return current;
}
