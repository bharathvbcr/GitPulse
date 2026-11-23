/**
 * Mavi's page-side state: thread records, host summaries, and the timelines
 * of the threads opened in this run. One instance for the app.
 *
 * Live events are applied only to timelines that are loaded (a thread opened
 * at least once this run); every thread's status comes from the host's
 * summary (`mavi-status`), so the rail never has to replay a thread it is not
 * showing.
 */
import { applyLine, emptyThread } from "./reduce";
import { changedPaths } from "./files";
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

  /** Starts listening and reads what the host already runs. Idempotent. */
  async init(): Promise<void> {
    if (this.#started) return;
    this.#started = true;
    this.records = readThreadRecords(storage());
    await onMaviEvents((id, entries) => this.#onEntries(id, entries));
    await onMaviStatus((summary) => this.#onSummary(summary));
    try {
      const live = await maviClient.list();
      const known = new Set(live.map((s) => s.id));
      for (const record of this.records) {
        if (record.sessionId && !known.has(record.sessionId)) record.sessionId = null;
      }
      for (const summary of live) this.#onSummary(summary);
      this.#persist();
    } catch {
      for (const record of this.records) record.sessionId = null;
    }
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

  #onSummary(summary: MaviSummary): void {
    this.summaries[summary.id] = summary;
    const record = this.recordFor(summary.id);
    if (record) {
      record.lastActivityAt = Math.max(record.lastActivityAt, summary.last_activity_ms);
      if (summary.conversation_id && record.conversationId !== summary.conversation_id) {
        record.conversationId = summary.conversation_id;
      }
      if (this.viewing && this.openKey === record.key) record.seenAt = Date.now();
      this.#persist();
      if (summary.phase === "idle" || summary.phase === "ready") void this.#flushQueue(record.key);
    }
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
    record.seenAt = Date.now();
    this.#persist();
    if (this.timelines[key] || !record.sessionId) {
      if (!this.timelines[key]) this.timelines[key] = emptyThread();
      return;
    }
    await this.#load(record);
  }

  async #load(record: ThreadRecord): Promise<void> {
    if (!record.sessionId) return;
    this.#loading.add(record.key);
    const timeline = this.timelines[record.key] ?? emptyThread();
    try {
      const replay = await maviClient.events(record.sessionId, timeline.lastSeq);
      this.timelines[record.key] = timeline;
      const live = this.timelines[record.key]!;
      for (const entry of replay.entries) applyLine(live, entry.line, entry.seq);
      this.#onSummary(replay.summary);
    } catch (error) {
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
    const summary = await maviClient.start(request);
    const target: MaviTarget = summary.target;
    const now = Date.now();
    const record: ThreadRecord = {
      key: newKey(),
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
    await this.send(record.key, options.firstMessage);
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
    delete this.errors[key];
    const summary = await maviClient.start(base);
    record.sessionId = summary.id;
    this.summaries[summary.id] = summary;
    const timeline = this.timelines[key] ?? emptyThread();
    // Sequence numbers restart with the new host session.
    timeline.lastSeq = 0;
    timeline.ended = null;
    timeline.items.push({
      kind: "notice",
      id: `resume:${summary.id}`,
      turn: timeline.turn,
      tone: "info",
      text: record.conversationId
        ? "Resumed the conversation. Earlier messages are in the agent's own transcript; this view continues from here."
        : "Started a new agent for this thread.",
    });
    this.timelines[key] = timeline;
    this.#persist();
  }

  /** Sends now when the agent is between turns, otherwise queues. */
  async send(key: string, text: string): Promise<void> {
    const record = this.record(key);
    if (!record?.sessionId) throw new Error("Resume this thread before sending.");
    const summary = this.summaries[record.sessionId];
    if (summary && (summary.phase === "working" || summary.phase === "approval")) {
      this.queues[key] = [...(this.queues[key] ?? []), text];
      return;
    }
    await maviClient.send(record.sessionId, text);
  }

  unqueue(key: string, index: number): void {
    const queue = this.queues[key];
    if (!queue) return;
    this.queues[key] = queue.filter((_, i) => i !== index);
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
    } catch (error) {
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
    this.records = this.records.filter((r) => r.key !== key);
    delete this.timelines[key];
    delete this.queues[key];
    if (this.openKey === key) this.openKey = null;
    this.#persist();
  }

  /** Records that the conversation now lives in a terminal tab, or no longer does. */
  markMoved(key: string, moved: boolean): void {
    const record = this.record(key);
    if (!record) return;
    record.movedToTerminalAt = moved ? Date.now() : null;
    this.#persist();
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
