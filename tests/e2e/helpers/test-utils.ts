import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const PROJECT_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
export const DEVCOUNCIL_ROOT = "/Users/bharath/Code/devtools/DevCouncil";

export interface TempDirManager {
  create: (prefix?: string) => string;
  cleanup: () => void;
}

export function createTempManager(): TempDirManager {
  const dirs: string[] = [];
  return {
    create: (prefix = "gitpulse-e2e-") => {
      const dir = mkdtempSync(path.join(tmpdir(), prefix));
      dirs.push(dir);
      return dir;
    },
    cleanup: () => {
      for (const dir of dirs) {
        try {
          if (existsSync(dir)) {
            rmSync(dir, { recursive: true, force: true });
          }
        } catch {
          // ignore cleanup failures in temp
        }
      }
      dirs.length = 0;
    },
  };
}

export interface CliResult {
  status: number | null;
  stdout: string;
  stderr: string;
}

export function runCommand(
  cmd: string,
  args: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv; timeout?: number } = {}
): CliResult {
  const opts: SpawnSyncOptionsWithStringEncoding = {
    cwd: options.cwd ?? PROJECT_ROOT,
    env: { ...process.env, ...options.env },
    encoding: "utf8",
    timeout: options.timeout ?? 15_000,
  };
  const res = spawnSync(cmd, args, opts);
  return {
    status: res.status,
    stdout: res.stdout || "",
    stderr: res.stderr || "",
  };
}

export function createFixtureGitRepo(
  dir: string,
  initialFiles: Record<string, string> = { "README.md": "# Fixture Repo\n" }
): void {
  const git = (...args: string[]) => {
    const res = runCommand("git", args, { cwd: dir });
    if (res.status !== 0) {
      throw new Error(`git ${args.join(" ")} failed: ${res.stderr}`);
    }
    return res.stdout.trim();
  };

  git("init", "-q");
  git("config", "user.email", "e2e-tester@example.com");
  git("config", "user.name", "E2E Tester");
  git("config", "commit.gpgsign", "false");

  for (const [relPath, content] of Object.entries(initialFiles)) {
    const full = path.join(dir, relPath);
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, content);
  }

  git("add", "-A");
  git("commit", "-q", "-m", "Initial fixture commit");
}

/**
 * Standard Schema constants defined in PROJECT.md and DevCouncil owner
 */
export const SCHEMA_CONSTANTS = Object.freeze({
  CURRENT_SCHEMA_VERSION: 29,
  MIN_READER_SCHEMA_VERSION: 29,
  DC_STORE_SCHEMA_VERSION: 11,
  WORKBENCH_SCHEMA_VERSION: 14,
  EXTRACTION_SCHEMA_VERSION: 98,
  ZSTD_MAGIC_HEADER: Buffer.from([0x28, 0xb5, 0x2f, 0xfd]),
});

/**
 * Priority Scale Mapping for Feature 13
 */
export type PriorityLevel = "P0" | "P1" | "P2" | "P3";

export function normalizePriority(raw: unknown): { code: PriorityLevel; rank: number } {
  if (typeof raw === "number") {
    switch (raw) {
      case 0:
        return { code: "P0", rank: 0 };
      case 1:
        return { code: "P1", rank: 1 };
      case 2:
        return { code: "P2", rank: 2 };
      case 3:
        return { code: "P3", rank: 3 };
      default:
        return { code: "P2", rank: 2 };
    }
  }

  const s = String(raw || "").trim().toLowerCase();
  if (s === "0" || s === "p0" || s === "critical" || s === "urgent") {
    return { code: "P0", rank: 0 };
  }
  if (s === "1" || s === "p1" || s === "high") {
    return { code: "P1", rank: 1 };
  }
  if (s === "2" || s === "p2" || s === "medium" || s === "normal") {
    return { code: "P2", rank: 2 };
  }
  if (s === "3" || s === "p3" || s === "low") {
    return { code: "P3", rank: 3 };
  }
  return { code: "P2", rank: 2 }; // Default safe fallback
}

/**
 * VirtualList calculation oracle for Feature 10
 */
export interface VirtualWindow {
  startIndex: number;
  endIndex: number;
  renderedCount: number;
  totalHeight: number;
  topPadding: number;
  bottomPadding: number;
}

export function computeVirtualWindow(
  totalItems: number,
  itemHeight: number,
  viewportHeight: number,
  scrollTop: number,
  buffer = 5
): VirtualWindow {
  if (totalItems <= 0 || itemHeight <= 0) {
    return {
      startIndex: 0,
      endIndex: 0,
      renderedCount: 0,
      totalHeight: 0,
      topPadding: 0,
      bottomPadding: 0,
    };
  }

  const effectiveScroll = Math.max(0, scrollTop);
  const totalHeight = totalItems * itemHeight;
  const rawStart = Math.floor(effectiveScroll / itemHeight);
  const rawVisible = Math.ceil(viewportHeight / itemHeight);

  const startIndex = Math.max(0, rawStart - buffer);
  const endIndex = Math.min(totalItems, rawStart + rawVisible + buffer);
  const renderedCount = Math.max(0, endIndex - startIndex);

  const topPadding = startIndex * itemHeight;
  const bottomPadding = Math.max(0, (totalItems - endIndex) * itemHeight);

  return {
    startIndex,
    endIndex,
    renderedCount,
    totalHeight,
    topPadding,
    bottomPadding,
  };
}

/**
 * Faceted Hydration simulator / oracle for Feature 6
 */
export interface FacetedHydrationResult {
  fileStatus: { path: string; status: string }[];
  branchGraph: { name: string; isHead: boolean }[];
  metadata: { tags: string[]; stashCount: number };
  invocations: Record<string, number>;
  decoupledErrors: string[];
}

export class FacetedHydrationPipeline {
  private gitInvocations: Record<string, number> = {
    fileStatus: 0,
    branchGraph: 0,
    metadata: 0,
  };

  async hydrateFileStatus(repoPath: string, fail = false): Promise<{ path: string; status: string }[]> {
    this.gitInvocations.fileStatus++;
    if (fail) throw new Error("FileStatusFacet failed");
    return [{ path: "src/main.rs", status: "modified" }];
  }

  async hydrateBranchGraph(repoPath: string, fail = false): Promise<{ name: string; isHead: boolean }[]> {
    this.gitInvocations.branchGraph++;
    if (fail) throw new Error("BranchGraphFacet failed");
    return [
      { name: "main", isHead: true },
      { name: "feature/1", isHead: false },
    ];
  }

  async hydrateMetadata(repoPath: string, fail = false): Promise<{ tags: string[]; stashCount: number }> {
    this.gitInvocations.metadata++;
    if (fail) throw new Error("MetadataFacet failed");
    return { tags: ["v1.0.0"], stashCount: 1 };
  }

  async runRoutineEditTick(repoPath: string): Promise<FacetedHydrationResult> {
    // A routine file edit only invokes FileStatusFacet (1 subprocess), not all 3
    const fileStatus = await this.hydrateFileStatus(repoPath);
    return {
      fileStatus,
      branchGraph: [],
      metadata: { tags: [], stashCount: 0 },
      invocations: { ...this.gitInvocations },
      decoupledErrors: [],
    };
  }

  async runFullHydration(
    repoPath: string,
    faults: { failMetadata?: boolean; failStatus?: boolean; failBranches?: boolean } = {}
  ): Promise<FacetedHydrationResult> {
    const decoupledErrors: string[] = [];
    let fileStatus: { path: string; status: string }[] = [];
    let branchGraph: { name: string; isHead: boolean }[] = [];
    let metadata: { tags: string[]; stashCount: number } = { tags: [], stashCount: 0 };

    const results = await Promise.allSettled([
      this.hydrateFileStatus(repoPath, faults.failStatus),
      this.hydrateBranchGraph(repoPath, faults.failBranches),
      this.hydrateMetadata(repoPath, faults.failMetadata),
    ]);

    if (results[0].status === "fulfilled") {
      fileStatus = results[0].value;
    } else {
      decoupledErrors.push(`fileStatus: ${results[0].reason?.message || "error"}`);
    }

    if (results[1].status === "fulfilled") {
      branchGraph = results[1].value;
    } else {
      decoupledErrors.push(`branchGraph: ${results[1].reason?.message || "error"}`);
    }

    if (results[2].status === "fulfilled") {
      metadata = results[2].value;
    } else {
      decoupledErrors.push(`metadata: ${results[2].reason?.message || "error"}`);
    }

    return {
      fileStatus,
      branchGraph,
      metadata,
      invocations: { ...this.gitInvocations },
      decoupledErrors,
    };
  }

  getInvocations(): Record<string, number> {
    return { ...this.gitInvocations };
  }
}

/**
 * Per-Repository Indexing Concurrency Lock Manager for Feature 7
 */
export class PerRepoBuildCoordinator {
  private activeBuilds = new Map<string, { startedAt: number }>();

  claim(repoPath: string): { ok: boolean; holder?: string } {
    const canonical = path.resolve(repoPath);
    if (this.activeBuilds.has(canonical)) {
      return { ok: false, holder: canonical };
    }
    this.activeBuilds.set(canonical, { startedAt: Date.now() });
    return { ok: true };
  }

  release(repoPath: string): boolean {
    const canonical = path.resolve(repoPath);
    return this.activeBuilds.delete(canonical);
  }

  isBuilding(repoPath: string): boolean {
    return this.activeBuilds.has(path.resolve(repoPath));
  }

  activeCount(): number {
    return this.activeBuilds.size;
  }
}

/**
 * Centralized Heartbeat Dispatcher simulator for Feature 8
 */
export interface Subscriber {
  id: string;
  intervalMs: number;
  callback: (tick: number) => void;
  lastTickMs: number;
}

export class CentralizedHeartbeatDispatcher {
  private subscribers = new Map<string, Subscriber>();
  private tickCount = 0;
  private isThrottled = false;

  subscribe(id: string, intervalMs: number, callback: (tick: number) => void): () => void {
    this.subscribers.set(id, {
      id,
      intervalMs,
      callback,
      lastTickMs: 0,
    });
    return () => {
      this.subscribers.delete(id);
    };
  }

  setThrottled(throttled: boolean): void {
    this.isThrottled = throttled;
  }

  step(currentVirtualTimeMs: number): number {
    this.tickCount++;
    let dispatchedCount = 0;

    for (const sub of this.subscribers.values()) {
      const effectiveInterval = this.isThrottled ? sub.intervalMs * 2 : sub.intervalMs;
      if (currentVirtualTimeMs - sub.lastTickMs >= effectiveInterval) {
        sub.lastTickMs = currentVirtualTimeMs;
        try {
          sub.callback(this.tickCount);
        } catch {
          // Fault isolation: subscriber errors do not halt the master ticker
        }
        dispatchedCount++;
      }
    }

    return dispatchedCount;
  }

  subscriberCount(): number {
    return this.subscribers.size;
  }
}

/**
 * Condvar Ingestion Synchronization Simulator for Feature 9
 */
export class CondvarLedgerIngestSync {
  private activeLedgerRepos = new Set<string>();
  private waitQueue: { repo: string; resolve: () => void }[] = [];

  acquire(ledgerRepo: string, timeoutMs: number): Promise<boolean> {
    return new Promise((resolve) => {
      if (!this.activeLedgerRepos.has(ledgerRepo)) {
        this.activeLedgerRepos.add(ledgerRepo);
        return resolve(true);
      }

      let timer: NodeJS.Timeout | null = null;
      let settled = false;

      const waiter = {
        repo: ledgerRepo,
        resolve: () => {
          if (settled) return;
          settled = true;
          if (timer) clearTimeout(timer);
          this.activeLedgerRepos.add(ledgerRepo);
          resolve(true);
        },
      };

      this.waitQueue.push(waiter);

      timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        const idx = this.waitQueue.indexOf(waiter);
        if (idx >= 0) this.waitQueue.splice(idx, 1);
        resolve(false);
      }, timeoutMs);
    });
  }

  release(ledgerRepo: string): void {
    this.activeLedgerRepos.delete(ledgerRepo);
    const nextIdx = this.waitQueue.findIndex((w) => w.repo === ledgerRepo);
    if (nextIdx >= 0) {
      const next = this.waitQueue.splice(nextIdx, 1)[0];
      next.resolve();
    }
  }

  isLocked(ledgerRepo: string): boolean {
    return this.activeLedgerRepos.has(ledgerRepo);
  }

  waitingCount(): number {
    return this.waitQueue.length;
  }
}

/**
 * View Section Panel DOM Retention Model for Feature 11
 */
export interface TabViewState {
  repoPath: string;
  scrollOffset: number;
  expandedFolders: Set<string>;
  activeDiffFile: string | null;
  domMounted: boolean;
  domVisible: boolean;
}

export class ViewSectionPanelRetentionManager {
  private tabs = new Map<string, TabViewState>();
  private activePath: string | null = null;

  openTab(repoPath: string): TabViewState {
    if (!this.tabs.has(repoPath)) {
      this.tabs.set(repoPath, {
        repoPath,
        scrollOffset: 0,
        expandedFolders: new Set(),
        activeDiffFile: null,
        domMounted: true,
        domVisible: false,
      });
    }
    return this.switchTab(repoPath);
  }

  switchTab(repoPath: string): TabViewState {
    if (this.activePath && this.tabs.has(this.activePath)) {
      // In non-keyed retention, DOM remains mounted, only visibility is toggled
      const prev = this.tabs.get(this.activePath)!;
      prev.domVisible = false;
    }
    const current = this.tabs.get(repoPath);
    if (!current) throw new Error(`Tab not found: ${repoPath}`);
    current.domVisible = true;
    this.activePath = repoPath;
    return current;
  }

  closeTab(repoPath: string): boolean {
    const tab = this.tabs.get(repoPath);
    if (!tab) return false;
    tab.domMounted = false;
    tab.domVisible = false;
    this.tabs.delete(repoPath);
    if (this.activePath === repoPath) {
      this.activePath = this.tabs.keys().next().value || null;
      if (this.activePath) {
        this.tabs.get(this.activePath)!.domVisible = true;
      }
    }
    return true;
  }

  getActiveTab(): TabViewState | null {
    return this.activePath ? this.tabs.get(this.activePath) || null : null;
  }

  getTabCount(): number {
    return this.tabs.size;
  }
}
