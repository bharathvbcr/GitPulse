/**
 * Browser sessions an agent asked for (`gitpulse_browser_open` →
 * `Request::Browse` → the `gitpulse-browse` event).
 *
 * Installed once at boot (`main.ts`), so a request is heard before any dock
 * has been opened. Several agents can ask at once, and the terminal's launch
 * channel takes one request at a time, so requests wait here in a bounded
 * queue and are handed over in order: open the repository, show its dock,
 * then ask its panel for the session (`terminalLaunchRequests.requestBrowser`,
 * which reuses a tab already on the address). A request that cannot be
 * carried out is said, never dropped in silence — the agent was told only
 * that GitPulse took it.
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { formatError } from "../ui/formatError";
import { TERMINAL_SESSION_PENDING } from "../terminal/sessionMessages";

export interface BrowseRequest {
  repo: string;
  url: string;
}

/** Requests that may wait at once; one past this is refused with a notice. */
export const MAX_QUEUED_BROWSE = 8;

/** How long a request waits for the launch channel to come free. */
const CHANNEL_WAIT_MS = 10_000;

/** A `gitpulse-browse` payload, or null for anything that is not one. */
export function parseBrowseRequest(payload: unknown): BrowseRequest | null {
  if (!payload || typeof payload !== "object") return null;
  const raw = payload as Record<string, unknown>;
  const { repo, url } = raw;
  if (typeof repo !== "string" || typeof url !== "string") return null;
  if (!repo || repo.length > 4096 || !url || url.length > 2048) return null;
  return { repo, url };
}

export interface BrowseDeps {
  /** Opens (or brings forward) the repository; its canonical path, or null. */
  open(repo: string): Promise<string | null>;
  /** Shows that repository's terminal dock. */
  showDock(): void;
  /** Whether the launch channel is holding another request. */
  busy(): boolean;
  /** Asks the repository's panel for the session; resolves once it opened. */
  launch(repo: string, url: string): Promise<void>;
  /** Says what did not happen. */
  fail(message: string): void;
  sleep?(ms: number): Promise<void>;
}

export function createBrowseQueue(deps: BrowseDeps) {
  const waiting: BrowseRequest[] = [];
  let draining = false;
  const sleep = deps.sleep ?? ((ms: number) => new Promise<void>((done) => setTimeout(done, ms)));

  async function handle(request: BrowseRequest): Promise<void> {
    const repo = await deps.open(request.repo);
    if (!repo) throw new Error(`Couldn't open ${request.repo}.`);
    deps.showDock();
    for (let waited = 0; deps.busy(); waited += 100) {
      if (waited >= CHANNEL_WAIT_MS) throw new Error(TERMINAL_SESSION_PENDING);
      await sleep(100);
    }
    await deps.launch(repo, request.url);
  }

  async function drain(): Promise<void> {
    if (draining) return;
    draining = true;
    try {
      for (let next = waiting.shift(); next; next = waiting.shift()) {
        try {
          await handle(next);
        } catch (cause) {
          deps.fail(`An agent's browser session for ${next.url.slice(0, 200)} did not open: ${formatError(cause)}`);
        }
      }
    } finally {
      draining = false;
    }
  }

  return {
    /** Queues `request`; false (and a notice) when the queue is full. */
    push(request: BrowseRequest): boolean {
      if (waiting.length >= MAX_QUEUED_BROWSE) {
        deps.fail(`An agent's browser session for ${request.url.slice(0, 200)} was not opened: ${MAX_QUEUED_BROWSE} requests are already waiting.`);
        return false;
      }
      waiting.push(request);
      void drain();
      return true;
    },
    /** Resolves once every queued request has been handled. */
    async settled(): Promise<void> {
      while (draining || waiting.length) await sleep(20);
    },
  };
}

/** Listens for agents' browser session requests for the life of the window. */
export async function installBrowseRequests(): Promise<UnlistenFn> {
  const [{ repoStore }, { toastStore }, { terminalLaunchRequests }] = await Promise.all([
    import("../stores/repoStore"),
    import("../stores/toastStore"),
    import("../terminal/launchRequests"),
  ]);
  const queue = createBrowseQueue({
    async open(repo) {
      let canonical: string | null = null;
      const opened = await repoStore.openRepo(repo, { activate: true, onReady: (path) => (canonical = path) });
      return opened ? (canonical ?? repo) : null;
    },
    showDock: () => repoStore.setTerminalOpen(true),
    busy: () => terminalLaunchRequests.busy,
    launch: (repo, url) => terminalLaunchRequests.requestBrowser(repo, url),
    fail: (message) => toastStore.error(message),
  });
  return listen("gitpulse-browse", (event) => {
    const request = parseBrowseRequest(event.payload);
    if (request) queue.push(request);
  });
}
