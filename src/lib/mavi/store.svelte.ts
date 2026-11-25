/**
 * Mavi's page-side state: thread records, host summaries, and the timelines
 * of the threads opened in this run. One instance for the app.
 *
 * Live events are applied only to timelines that are loaded (a thread opened
 * at least once this run); every thread's status comes from the host's
 * summary (`mavi-status`), so the rail never has to replay a thread it is not
 * showing.
 */
import { applyLine, continueInNewSession, emptyThread } from "./reduce";
import { changedPaths } from "./files";
import { childTabs, closeThreadChildren } from "./childTabs.svelte";
import { maviClient, onMaviEvents, onMaviStatus, type MaviModeId, type StartRequest } from "./client";
import {
  readThreadRecords,
  threadStatus,
  titleFrom,
  writeThreadRecords,
  type ThreadRecord,
  type ThreadStatus,
} from "./threads";
import type { MaviEntry, MaviSummary, MaviTarget, ThreadState } from "./types";

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

const UNSENT_KEY = "gitpulse.mavi.unsent";

/**
 * Unsent words per thread: the composer's draft and messages still queued.
 * Kept across a quit, which ends every agent: what was queued comes back as
 * the thread's draft, never sent by itself.
 */
function readUnsent(store: Storage | null): Record<string, string> {
  try {
    const raw = store?.getItem(UNSENT_KEY);
    const value: unknown = raw ? JSON.parse(raw) : null;
    if (!value || typeof value !== "object") return {};
    const out: Record<string, string> = {};
    for (const [key, text] of Object.entries(value as Record<string, unknown>)) {
      if (typeof text === "string" && text.trim()) out[key] = text.slice(0, 100_000);
    }
    return out;
  } catch {
    return {};
  }
}

function newKey(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `t${Date.now()}${Math.random().toString(36).slice(2)}`;
  }
}

export interface NewThreadOptions {
  target: { kind: "local"; repoPath: string } | { kind: "ssh"; host: string; dir: string };
  mode: MaviModeId;
  acknowledged?: boolean;
  model?: string;
  firstMessage: string;
}

class MaviStore {
  records = $state<ThreadRecord[]>([]);
  summaries = $state<Record<string, MaviSummary>>({});
  timelines = $state<Record<string, ThreadState>>({});
  /** Messages written while a turn runs, sent in order when it ends. */
  queues = $state<Record<string, string[]>>({});
  /** Unsent composer text per thread, so switching threads never moves it. */
  drafts = $state<Record<string, string>>({});
  openKey = $state<string | null>(null);
  /**
   * The Mavi view is on screen (mounted, and the window visible). Only then
   * does the open thread count as seen: being the last thread opened, while
   * the person reads Explorer, is not seeing it.
   */
  viewing = $state(false);
  /** A request from elsewhere in the app to show the new-thread form. */
  composeRequest = $state<{ id: number; message: string; newWorktree: boolean } | null>(null);
  errors = $state<Record<string, string>>({});
  #started = false;
  #buffer = new Map<string, MaviEntry[]>();
  #loading = new Set<string>();
  #flushing = new Set<string>();
  #loads = new Map<string, Promise<void>>();
  #init: Promise<void> | null = null;

  /**
   * Starts listening and reads what the host already runs. Idempotent; a
   * failed start can be tried again, and every caller sees its error.
   */
  init(): Promise<void> {
    this.#init ??= this.#start().catch((error) => {
      this.#init = null;
      throw error;
    });
    return this.#init;
  }

  async #start(): Promise<void> {
    if (!this.#started) {
      this.records = readThreadRecords(storage());
      const known = new Set(this.records.map((r) => r.key));
      for (const [key, text] of Object.entries(readUnsent(storage()))) {
        if (known.has(key) && !this.drafts[key]) this.drafts[key] = text;
      }
      await onMaviEvents((id, entries) => this.#onEntries(id, entries));
      await onMaviStatus((summary) => this.#onSummary(summary));
      this.#started = true;
    }
    // When the host cannot be asked, sessions are left as they were: calling
    // them "not running" would invite a second agent on a live conversation.
    const live = await maviClient.list();
    const known = new Set(live.map((s) => s.id));
    for (const record of this.records) {
      if (record.sessionId && !known.has(record.sessionId)) record.sessionId = null;
    }
    for (const summary of live) this.#onSummary(summary);
    this.#persist();
  }

  recordFor(sessionId: string): ThreadRecord | undefined {
    return this.records.find((r) => r.sessionId === sessionId);
  }

  record(key: string): ThreadRecord | undefined {
    return this.records.find((r) => r.key === key);
  }

  summaryOf(record: ThreadRecord): MaviSummary | null {
    return record.sessionId ? (this.summaries[record.sessionId] ?? null) : null;
  }

  status(record: ThreadRecord): ThreadStatus {
    return threadStatus(record, this.summaryOf(record), this.timelines[record.key] ?? null, this.viewing && this.openKey === record.key);
  }

  #persist(): void {
    writeThreadRecords(storage(), this.records);
  }

  /** Writes every thread's unsent words: its draft, then anything queued. */
  #saveUnsent(): void {
    const out: Record<string, string> = {};
    for (const record of this.records) {
      // Queued messages first: they were written before the draft.
      const queued = this.queues[record.key] ?? [];
      const draft = this.drafts[record.key]?.trim();
      const parts = [...queued, ...(draft ? [draft] : [])];
      if (parts.length) out[record.key] = parts.join("\n\n");
    }
    try {
      const store = storage();
      if (Object.keys(out).length) store?.setItem(UNSENT_KEY, JSON.stringify(out));
      else store?.removeItem(UNSENT_KEY);
    } catch {
      /* storage full or blocked: in memory only */
    }
  }

  /** The composer's text for a thread changed. */
  setDraft(key: string, text: string): void {
    if (text) this.drafts[key] = text;
    else delete this.drafts[key];
    this.#saveUnsent();
  }

  #onSummary(summary: MaviSummary): void {
    this.summaries[summary.id] = summary;
    const record = this.recordFor(summary.id);
    if (record) {
      let changed = false;
      if (summary.last_activity_ms > record.lastActivityAt) {
        record.lastActivityAt = summary.last_activity_ms;
        changed = true;
      }
      if (summary.conversation_id && record.conversationId !== summary.conversation_id) {
        record.conversationId = summary.conversation_id;
        changed = true;
      }
      if (this.viewing && this.openKey === record.key) {
        record.seenAt = Date.now();
        changed = true;
      }
      // Status lines arrive often; storage is written only when a record moved.
      if (changed) this.#persist();
      if (summary.phase === "idle" || summary.phase === "ready") void this.#flushQueue(record.key);
      if (summary.phase === "ended") this.#returnQueue(record.key);
    }
  }

  /**
   * The agent ended with messages still queued: they go back to the composer
   * as a draft, where the person sees them, instead of being sent by surprise
   * after a resume.
   */
  #returnQueue(key: string): void {
    const queue = this.queues[key];
    if (!queue?.length) return;
    const draft = this.drafts[key]?.trim();
    this.drafts[key] = [...queue, ...(draft ? [draft] : [])].join("\n\n");
    delete this.queues[key];
    this.#saveUnsent();
  }

  #fileListeners = new Set<(record: ThreadRecord, path: string) => void>();

  /**
   * Called with a repository-relative path each time a local thread's agent
   * applies an edit, and for each path that moved outside its edit tools when
   * a turn ends. Every thread, opened or not: the dock reloads open panes.
   */
  onFileChanged(listener: (record: ThreadRecord, path: string) => void): () => void {
    this.#fileListeners.add(listener);
    return () => this.#fileListeners.delete(listener);
  }

  #announceFiles(record: ThreadRecord, entries: MaviEntry[]): void {
    if (this.#fileListeners.size === 0 || record.target.kind !== "local") return;
    for (const path of changedPaths(record.target.root, entries)) {
      for (const listener of this.#fileListeners) listener(record, path);
    }
  }

  #onEntries(sessionId: string, entries: MaviEntry[]): void {
    const record = this.recordFor(sessionId);
    if (!record) return;
    this.#announceFiles(record, entries);
    if (this.#loading.has(record.key)) {
      const pending = this.#buffer.get(record.key) ?? [];
      pending.push(...entries);
      this.#buffer.set(record.key, pending);
      return;
    }
    const timeline = this.timelines[record.key];
    if (!timeline) return;
    for (const entry of entries) applyLine(timeline, entry.line, entry.seq);
  }

  /** Opens a thread: replays what the host kept, then follows it live. */
  async open(key: string): Promise<void> {
    this.openKey = key;
    const record = this.record(key);
    if (!record) return;
    if (this.viewing) {
      record.seenAt = Date.now();
      this.#persist();
    }
    if (this.timelines[key] || !record.sessionId) {
      if (!this.timelines[key]) this.timelines[key] = emptyThread();
      return;
    }
    await this.#load(record);
  }

  /** Replays a thread's session once at a time: a second caller joins the first. */
  #load(record: ThreadRecord): Promise<void> {
    const running = this.#loads.get(record.key);
    if (running) return running;
    const load = this.#replay(record).finally(() => this.#loads.delete(record.key));
    this.#loads.set(record.key, load);
    return load;
  }

  async #replay(record: ThreadRecord): Promise<void> {
    if (!record.sessionId) return;
    this.#loading.add(record.key);
    const timeline = this.timelines[record.key] ?? emptyThread();
    try {
      const replay = await maviClient.events(record.sessionId, timeline.lastSeq);
      // Forgotten while the replay was on its way: nothing to fill.
      if (!this.record(record.key)) return;
      this.timelines[record.key] = timeline;
      const live = this.timelines[record.key]!;
      for (const entry of replay.entries) applyLine(live, entry.line, entry.seq);
      this.#onSummary(replay.summary);
    } catch (error) {
      if (!this.record(record.key)) return;
      this.timelines[record.key] = timeline;
      this.errors[record.key] = error instanceof Error ? error.message : String(error);
    } finally {
      this.#loading.delete(record.key);
      const buffered = this.#buffer.get(record.key) ?? [];
      this.#buffer.delete(record.key);
      const live = this.timelines[record.key];
      if (live) for (const entry of buffered) applyLine(live, entry.line, entry.seq);
    }
  }

  async create(options: NewThreadOptions): Promise<string> {
    const request: StartRequest =
      options.target.kind === "local"
        ? { repoPath: options.target.repoPath, mode: options.mode }
        : { host: options.target.host, dir: options.target.dir, mode: options.mode };
    if (options.acknowledged) request.acknowledged = true;
    if (options.model) request.model = options.model;
    // The key exists before the agent does: the host names the agent's tabs by it.
    const threadKey = newKey();
    request.threadKey = threadKey;
    const summary = await maviClient.start(request);
    const target: MaviTarget = summary.target;
    const now = Date.now();
    const record: ThreadRecord = {
      key: threadKey,
      sessionId: summary.id,
      title: titleFrom(options.firstMessage),
      target,
      mode: options.mode,
      model: options.model ?? null,
      conversationId: null,
      createdAt: now,
      lastActivityAt: now,
      seenAt: now,
      movedToTerminalAt: null,
    };
    this.records = [record, ...this.records];
    this.summaries[summary.id] = summary;
    this.timelines[record.key] = emptyThread();
    this.#persist();
    this.openKey = record.key;
    // Anything the agent printed before the record existed (a startup hook).
    await this.#load(record);
    try {
      await this.send(record.key, options.firstMessage);
    } catch (error) {
      // The thread exists either way: a retry from the form would start a
      // second agent (and, with a new worktree, a second worktree). The
      // message waits in the thread's composer with the reason beside it.
      this.drafts[record.key] = options.firstMessage;
      this.#saveUnsent();
      this.errors[record.key] = error instanceof Error ? error.message : String(error);
    }
    return record.key;
  }

  /** Starts a new agent for a record whose agent is not running, resuming its conversation. */
  async resume(key: string, acknowledged = false): Promise<void> {
    const record = this.record(key);
    if (!record) return;
    const base: StartRequest =
      record.target.kind === "local"
        ? { repoPath: record.target.root, mode: record.mode as MaviModeId }
        : { host: record.target.host, dir: record.target.dir, mode: record.mode as MaviModeId };
    if (record.conversationId) base.resume = record.conversationId;
    if (record.model) base.model = record.model;
    if (acknowledged) base.acknowledged = true;
    base.threadKey = key;
    delete this.errors[key];
    const summary = await maviClient.start(base);
    record.sessionId = summary.id;
    this.summaries[summary.id] = summary;
    if (record.conversationId) {
      // The host seeds the conversation's earlier turns from Claude Code's
      // transcript into the new session, so the timeline is rebuilt from that
      // replay instead of keeping this run's partial copy beside it.
      this.timelines[key] = emptyThread();
      await this.#load(record);
    } else {
      if (!this.timelines[key]) this.timelines[key] = emptyThread();
      const timeline = this.timelines[key]!;
      // Sequence numbers, turns and cost restart with the new host session;
      // this run's turns stay above it.
      continueInNewSession(timeline);
      timeline.items.push({
        kind: "notice",
        id: `resume:${summary.id}`,
        turn: timeline.turn,
        tone: "info",
        text: "Started a new agent for this thread.",
      });
      // What it printed before this record pointed at it (a startup hook).
      await this.#load(record);
    }
    this.#persist();
  }

  /**
   * Takes over a conversation that ran elsewhere (a terminal tab): starts an
   * agent resuming it in `root`, with its earlier turns read from the
   * transcript, and opens the thread. The caller has ended the other side.
   */
  async adopt(options: {
    root: string;
    conversationId: string;
    mode: MaviModeId;
    acknowledged?: boolean;
    title?: string;
  }): Promise<string> {
    const request: StartRequest = { repoPath: options.root, mode: options.mode, resume: options.conversationId };
    if (options.acknowledged) request.acknowledged = true;
    // A thread this conversation already had keeps its key (and its tabs);
    // otherwise the key minted here becomes the new thread's.
    const known = this.records.find(
      (r) =>
        r.conversationId?.toLowerCase() === options.conversationId.toLowerCase() &&
        r.target.kind === "local" &&
        r.target.root === options.root,
    );
    const mintedKey = known?.key ?? newKey();
    request.threadKey = mintedKey;
    const summary = await maviClient.start(request);
    const startedIn = summary.target.kind === "local" ? summary.target.root : null;
    const existing = this.records.find(
      (r) =>
        r.conversationId?.toLowerCase() === options.conversationId.toLowerCase() &&
        r.target.kind === "local" &&
        r.target.root === startedIn,
    );
    const now = Date.now();
    let record: ThreadRecord;
    let key: string;
    if (existing) {
      // The conversation was a Mavi thread before it moved to the terminal.
      record = existing;
      record.sessionId = summary.id;
      record.mode = options.mode;
      record.movedToTerminalAt = null;
      record.lastActivityAt = now;
      key = record.key;
    } else {
      key = mintedKey;
      record = {
        key,
        sessionId: summary.id,
        title: options.title?.trim() || "Continued from a terminal",
        target: summary.target,
        mode: options.mode,
        model: null,
        conversationId: options.conversationId.toLowerCase(),
        createdAt: now,
        lastActivityAt: now,
        seenAt: now,
        movedToTerminalAt: null,
      };
      this.records = [record, ...this.records];
    }
    this.summaries[summary.id] = summary;
    this.timelines[key] = emptyThread();
    this.openKey = key;
    await this.#load(this.record(key) ?? record);
    // A thread adopted under a placeholder title takes its first prompt's.
    // Written through the store's own copy: a write to the object passed in
    // would not reach the page.
    const stored = this.record(key);
    if (stored && !existing && !options.title?.trim()) {
      const first = this.timelines[key]?.items.find((i) => i.kind === "user");
      if (first && first.kind === "user") stored.title = titleFrom(first.text);
    }
    this.#persist();
    return key;
  }

  /** Sends now when the agent is between turns, otherwise queues. */
  async send(key: string, text: string): Promise<void> {
    const record = this.record(key);
    if (!record?.sessionId) throw new Error("Resume this thread before sending.");
    const summary = this.summaries[record.sessionId];
    if (summary && (summary.phase === "working" || summary.phase === "approval")) {
      this.queues[key] = [...(this.queues[key] ?? []), text];
      this.#saveUnsent();
      return;
    }
    await maviClient.send(record.sessionId, text);
  }

  unqueue(key: string, index: number): void {
    const queue = this.queues[key];
    if (!queue) return;
    this.queues[key] = queue.filter((_, i) => i !== index);
    this.#saveUnsent();
  }

  async #flushQueue(key: string): Promise<void> {
    if (this.#flushing.has(key)) return;
    const queue = this.queues[key];
    const record = this.record(key);
    if (!queue?.length || !record?.sessionId) return;
    this.#flushing.add(key);
    try {
      // All queued messages go as one turn, in the order they were written.
      const text = queue.join("\n\n");
      this.queues[key] = [];
      await maviClient.send(record.sessionId, text);
      delete this.errors[key];
      this.#saveUnsent();
    } catch (error) {
      // Not sent (a turn the agent started itself, say): the messages go back
      // to the front of the queue, ahead of anything written since.
      this.queues[key] = [...queue, ...(this.queues[key] ?? [])];
      this.#saveUnsent();
      this.errors[key] = error instanceof Error ? error.message : String(error);
    } finally {
      this.#flushing.delete(key);
    }
  }

  async answer(key: string, requestId: string, decision: "allow" | "deny", message?: string): Promise<void> {
    const record = this.record(key);
    if (!record?.sessionId) return;
    await maviClient.answer(record.sessionId, requestId, decision, message);
  }

  async interrupt(key: string): Promise<void> {
    const record = this.record(key);
    if (record?.sessionId) await maviClient.interrupt(record.sessionId);
  }

  async stop(key: string): Promise<void> {
    const record = this.record(key);
    if (record?.sessionId) await maviClient.stop(record.sessionId);
  }

  forget(key: string): void {
    const record = this.record(key);
    if (record?.sessionId && this.summaries[record.sessionId]?.phase !== "ended") void maviClient.stop(record.sessionId);
    // Forgetting a thread ends its terminal tabs too; a stop or interrupt does not.
    if (record) {
      childTabs.forget(key);
      void closeThreadChildren(key).catch(() => {});
    }
    this.records = this.records.filter((r) => r.key !== key);
    delete this.timelines[key];
    delete this.queues[key];
    delete this.drafts[key];
    this.#saveUnsent();
    delete this.errors[key];
    this.#buffer.delete(key);
    if (record?.sessionId) delete this.summaries[record.sessionId];
    if (this.openKey === key) this.openKey = null;
    this.#persist();
  }

  /** Records that the conversation now lives in a terminal tab, or no longer does. */
  markMoved(key: string, moved: boolean): void {
    const record = this.record(key);
    if (!record) return;
    if (!!record.movedToTerminalAt === moved) return;
    record.movedToTerminalAt = moved ? Date.now() : null;
    this.#persist();
    // Said in the conversation too, so the agent's exit that follows reads
    // as the hand-off it is, not as a failure.
    const timeline = this.timelines[key];
    timeline?.items.push({
      kind: "notice",
      id: `moved:${record.movedToTerminalAt ?? Date.now()}:${moved ? 1 : 0}`,
      turn: timeline.turn,
      tone: "info",
      text: moved ? "Moved to a Claude Code tab in the terminal dock; the agent here ends." : "The move to the terminal did not happen; the conversation stays here.",
    });
  }

  /** Waits until the host says the thread's agent has exited, up to `ms`. */
  async waitForEnd(key: string, ms = 5000): Promise<boolean> {
    const started = Date.now();
    for (;;) {
      const record = this.record(key);
      const summary = record ? this.summaryOf(record) : null;
      if (!summary || summary.phase === "ended") return true;
      if (Date.now() - started > ms) return false;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }

  rename(key: string, title: string): void {
    const record = this.record(key);
    if (!record) return;
    record.title = title.trim().slice(0, 200) || record.title;
    this.#persist();
  }

  setViewing(on: boolean): void {
    this.viewing = on;
    const record = on && this.openKey ? this.record(this.openKey) : undefined;
    if (record) {
      record.seenAt = Date.now();
      this.#persist();
    }
  }

  requestCompose(message: string, options: { newWorktree?: boolean } = {}): void {
    this.composeRequest = {
      id: (this.composeRequest?.id ?? 0) + 1,
      message,
      newWorktree: options.newWorktree === true,
    };
  }

  /** How many threads need the reader, for the header badge and the dock pill. */
  attention(): { needsYou: number; failed: number; firstNeedsYou: string | null } {
    let needsYou = 0;
    let failed = 0;
    let firstNeedsYou: string | null = null;
    for (const record of this.records) {
      const status = this.status(record);
      if (status === "needs_you") {
        needsYou += 1;
        firstNeedsYou ??= record.key;
      } else if (status === "failed") {
        failed += 1;
      }
    }
    return { needsYou, failed, firstNeedsYou };
  }

  /** Files running threads in `root` have edited, for the Explorer's Live pulse. */
  touchedIn(root: string): { path: string; key: string; title: string; working: boolean }[] {
    const out: { path: string; key: string; title: string; working: boolean }[] = [];
    const norm = (p: string) => p.replace(/[\\/]+$/, "");
    for (const record of this.records) {
      if (record.target.kind !== "local" || norm(record.target.root) !== norm(root)) continue;
      const summary = this.summaryOf(record);
      if (!summary || summary.phase === "ended") continue;
      const working = summary.phase === "working" || summary.phase === "approval";
      for (const path of summary.touched) out.push({ path, key: record.key, title: record.title, working });
    }
    return out;
  }
}

export const maviStore = new MaviStore();
