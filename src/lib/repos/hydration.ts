import { parseTagList, type BranchInfo, type TagInfo } from "../branches/types";
import { ipcErrorKind } from "../ipc/invoke";
import type { InvokeFn } from "../stores/graphStore";
import { describeError } from "../ui/copy/describeFailure";
import type { RepoChange } from "./events";
import type { OperationState, RepoOperation } from "./operation";
import type { createSessionBook } from "./sessionBook";
import { parseStashList, type StashEntry } from "./stash";
import type {
  BranchStatsReport,
  FileStatus,
  RepoSession,
  ResolvedRepo,
} from "./storeTypes";
import { needsFullPoll } from "./watchState";

/**
 * Upper bound of cmd_branch_stats batches drained per fetch. The backend
 * computes at most 96 unique uncached tips per call, so 64 batches cover
 * ~6100 unique tips; past the bound draining stops silently and churn
 * resumes on the next refresh.
 */
export const STATS_DRAIN_MAX_BATCHES = 64;

/**
 * How recent a landed snapshot must be for activating its tab to skip the read
 * (given a live watch). Short because the skip trusts the watcher for the
 * interval; long enough to cover switching back and forth between two tabs.
 */
export const ACTIVATION_FRESH_MS = 5_000;

/** Publish coalesced stats every N batches (and always on the final drain). */
export const STATS_PUBLISH_EVERY = 8;

export interface FileStatusFacetPayload {
  statuses: FileStatus[];
  statusPollFailures: number;
  statusUnavailableSince: number | null;
}

export interface BranchGraphFacetPayload {
  branches: BranchInfo[];
  currentBranch: string | null;
  defaultBranch: string | null;
}

export interface MetadataFacetPayload {
  tags: TagInfo[];
  tagsTruncated: boolean;
  tagsFailed: boolean;
  operation: OperationState;
  stashEntries: StashEntry[];
  stashFailed: boolean;
  stashTruncated: boolean;
  fetchedAt: number | null;
}

export type HydrationFacet = "status" | "branches" | "metadata" | "all";

export function facetForChange(change?: RepoChange | null): HydrationFacet {
  if (!change || !change.kinds || change.kinds.length === 0) return "all";
  const kinds = change.kinds;
  const hasStatus = kinds.some(
    (k) => k === "worktree" || k === "index" || k === "documents" || k === "ignore",
  );
  const hasBranches = kinds.some(
    (k) => k === "refs" || k === "git_state" || k === "objects",
  );
  const hasMeta = kinds.some((k) => k === "config");

  if (hasStatus && !hasBranches && !hasMeta) return "status";
  if (hasBranches && !hasStatus && !hasMeta) return "branches";
  if (hasMeta && !hasStatus && !hasBranches) return "metadata";
  return "all";
}

export function mergeFacets(
  existing: HydrationFacet | undefined,
  incoming: HydrationFacet,
): HydrationFacet {
  if (!existing || existing === incoming) return incoming;
  if (existing === "all" || incoming === "all") return "all";
  return "all";
}

/**
 * True when the rejection is the backend refusing to run in a repository that
 * has not been approved (`repository_trust_required`). The kind is the whole
 * answer: a wrapped or reworded message neither makes nor hides it.
 */
export function repositoryTrustRefused(cause: unknown): boolean {
  return ipcErrorKind(cause) === "repository_trust_required";
}

export function branchStatsKey(
  name: string,
  isRemote: boolean,
  remoteName?: string | null,
): string {
  return `${isRemote ? "remote" : "local"}:${remoteName ?? ""}:${name}`;
}

/**
 * What the churn of a branch list was measured against: every branch and the
 * tip it points at. Churn is relative to the default branch, so a moved
 * default invalidates every other row's numbers even though their own tips
 * did not move — which is why the whole list is the signature, not each row's
 * tip on its own.
 */
export function branchTipSignature(branches: readonly BranchInfo[]): string {
  return branches
    .map((b) => `${branchStatsKey(b.name, b.is_remote, b.remote_name)}@${b.tip_commit_id}${b.is_default ? "*" : ""}`)
    .sort()
    .join("\n");
}

export type HydrationSessionPatch =
  | Partial<RepoSession>
  | ((current: RepoSession) => Partial<RepoSession>);

export interface HydrationDeps {
  invoke: InvokeFn;
  sessionById(id: string): RepoSession | undefined;
  applyToSession(id: string, generation: number, patchOrFn: HydrationSessionPatch): boolean;
  sessionBook: ReturnType<typeof createSessionBook>;
  onContentRevision(path: string, revision: string): void;
  now?: () => number;
}

/**
 * Owns faceted repository snapshot reads, churn carrying across refreshes,
 * repository-family resolution fallback, and progressive `cmd_branch_stats` draining.
 */
export function createHydrationController(deps: HydrationDeps) {
  const { invoke: invokeFn, sessionBook } = deps;
  const { familyUnknown, snapshotRuns, statsInflight, statsDrained } = sessionBook;
  const now = deps.now ?? (() => Date.now());

  async function loadFileStatusFacet(path: string): Promise<FileStatusFacetPayload> {
    const statuses = await invokeFn<FileStatus[]>("cmd_get_status", { repoPath: path });
    return {
      statuses,
      statusPollFailures: 0,
      statusUnavailableSince: null,
    };
  }

  async function loadBranchGraphFacet(path: string): Promise<BranchGraphFacetPayload> {
    const branches = await invokeFn<BranchInfo[]>("cmd_list_branches", { repoPath: path });
    const currentBranch = branches.find((b) => b.is_current)?.name || null;
    const defaultBranch =
      branches.find((b) => b.is_default)?.name || currentBranch || "main";
    return {
      branches,
      currentBranch,
      defaultBranch,
    };
  }

  async function loadMetadataFacet(path: string): Promise<MetadataFacetPayload> {
    const [tags, operation, stash, fetchedAt] = await Promise.all([
      invokeFn<unknown>("cmd_list_tags", { repoPath: path })
        .then((raw) => parseTagList(raw))
        .catch(() => ({ tags: [] as TagInfo[], truncated: false, failed: true })),
      invokeFn<RepoOperation | null>("cmd_repo_operation", { repoPath: path })
        .then((value) => ({ operation: value ?? null, probeFailed: false }))
        .catch(() => ({ operation: null, probeFailed: true })),
      invokeFn<unknown>("cmd_stash_list", { repoPath: path })
        .then((raw) => ({ ...parseStashList(raw), failed: false }))
        .catch(() => ({ entries: [] as StashEntry[], failed: true, truncated: false })),
      invokeFn<number | null>("cmd_last_fetch_at", { repoPath: path })
        .then((value) => (typeof value === "number" ? value : null))
        .catch(() => null),
    ]);
    return {
      tags: tags.tags,
      tagsTruncated: tags.truncated,
      tagsFailed: tags.failed,
      operation,
      stashEntries: stash.entries,
      stashFailed: stash.failed,
      stashTruncated: stash.truncated,
      fetchedAt,
    };
  }

  async function loadSnapshot(
    path: string,
  ): Promise<FileStatusFacetPayload & BranchGraphFacetPayload & MetadataFacetPayload> {
    const [fileStatus, branchGraph, metadata] = await Promise.all([
      loadFileStatusFacet(path),
      loadBranchGraphFacet(path),
      loadMetadataFacet(path),
    ]);
    return {
      ...fileStatus,
      ...branchGraph,
      ...metadata,
    };
  }

  async function resolvePath(path: string): Promise<ResolvedRepo> {
    return invokeFn<ResolvedRepo>("cmd_resolve_repo", { repoPath: path });
  }

  function resolvedFields(id: string, resolved: ResolvedRepo) {
    familyUnknown.delete(id);
    return {
      path: resolved.path,
      name: resolved.name,
      isBare: resolved.is_bare,
      commonDir: resolved.common_dir ?? null,
    };
  }

  function withCarriedChurn<T extends { branches: BranchInfo[] }>(
    id: string,
    live: RepoSession,
    snapshot: T,
  ): { patch: T; drainVerified: boolean } {
    if (live.branches.length === 0) return { patch: snapshot, drainVerified: false };
    const liveByKey = new Map(
      live.branches.map((b) => [branchStatsKey(b.name, b.is_remote, b.remote_name), b]),
    );

    const liveReady = !live.statsPending && !live.statsFailed;
    const drainedRecord = statsDrained.get(id);
    const wasCleanlyDrained =
      liveReady &&
      !statsInflight.has(id) &&
      drainedRecord?.signature === branchTipSignature(live.branches);
    const liveDefault = live.branches.find((b) => !b.is_remote && b.is_default)?.name ?? null;
    const snapDefault = snapshot.branches.find((b) => !b.is_remote && b.is_default)?.name ?? null;
    const drainedBase =
      wasCleanlyDrained && typeof drainedRecord?.comparedTo === "string" && drainedRecord.comparedTo.length > 0
        ? drainedRecord.comparedTo
        : undefined;
    const measuredBase =
      live.branches.find(
        (b) => typeof b.compared_to === "string" && b.compared_to.length > 0,
      )?.compared_to ??
      drainedBase ??
      liveDefault;
    const findNamedRef = (list: BranchInfo[], refName: string) =>
      list.find((b) => b.name === refName);
    const liveBase = measuredBase ? findNamedRef(live.branches, measuredBase) : undefined;
    const snapBase = measuredBase ? findNamedRef(snapshot.branches, measuredBase) : undefined;
    const baseStable =
      liveReady &&
      !!measuredBase &&
      !!liveBase &&
      !!snapBase &&
      liveBase.tip_commit_id === snapBase.tip_commit_id &&
      liveDefault === snapDefault;

    const liveLocalNames = new Set(
      live.branches.filter((b) => !b.is_remote).map((b) => b.name),
    );
    const snapLocalNames = new Set(
      snapshot.branches.filter((b) => !b.is_remote).map((b) => b.name),
    );
    const strippedRemoteName = (b: BranchInfo) =>
      b.remote_name && b.name.startsWith(`${b.remote_name}/`)
        ? b.name.slice(b.remote_name.length + 1)
        : b.name;

    const donorByTip = new Map<string, BranchInfo>();
    if (baseStable && measuredBase) {
      for (const b of live.branches) {
        if (
          !b.is_remote &&
          b.compared_to === measuredBase &&
          typeof b.additions === "number" &&
          !donorByTip.has(b.tip_commit_id)
        ) {
          donorByTip.set(b.tip_commit_id, b);
        }
      }
      for (const b of live.branches) {
        if (
          b.is_remote &&
          !liveLocalNames.has(strippedRemoteName(b)) &&
          b.compared_to === measuredBase &&
          typeof b.additions === "number" &&
          !donorByTip.has(b.tip_commit_id)
        ) {
          donorByTip.set(b.tip_commit_id, b);
        }
      }
    }

    let allEligibleVerified = wasCleanlyDrained && baseStable;
    const branches = snapshot.branches.map((b) => {
      const isExempt =
        (!b.is_remote && b.is_default) ||
        (!!measuredBase && b.name === measuredBase) ||
        (b.is_remote && snapLocalNames.has(strippedRemoteName(b)));
      const newlyUnshadowedRemote =
        b.is_remote &&
        liveLocalNames.has(strippedRemoteName(b)) &&
        !snapLocalNames.has(strippedRemoteName(b));

      const carried = liveByKey.get(branchStatsKey(b.name, b.is_remote, b.remote_name));
      const carriedBaseMoved = (() => {
        if (!carried?.compared_to) return false;
        const prevRef = findNamedRef(live.branches, carried.compared_to);
        const nextRef = findNamedRef(snapshot.branches, carried.compared_to);
        return !!prevRef && !!nextRef && prevRef.tip_commit_id !== nextRef.tip_commit_id;
      })();
      if (
        carried &&
        carried.tip_commit_id === b.tip_commit_id &&
        !newlyUnshadowedRemote &&
        !carriedBaseMoved
      ) {
        return {
          ...b,
          additions: carried.additions,
          deletions: carried.deletions,
          files_changed: carried.files_changed,
          commits_ahead_of_base: carried.commits_ahead_of_base,
          commits_behind_base: carried.commits_behind_base,
          compared_to: carried.compared_to,
        };
      }
      if (baseStable && measuredBase && snapBase && !isExempt) {
        const donor = donorByTip.get(b.tip_commit_id);
        if (donor) {
          return {
            ...b,
            additions: donor.additions,
            deletions: donor.deletions,
            files_changed: donor.files_changed,
            commits_ahead_of_base: donor.commits_ahead_of_base,
            commits_behind_base: donor.commits_behind_base,
            compared_to: donor.compared_to,
          };
        }
        if (b.tip_commit_id === snapBase.tip_commit_id) {
          return {
            ...b,
            additions: 0,
            deletions: 0,
            files_changed: 0,
            commits_ahead_of_base: 0,
            commits_behind_base: 0,
            compared_to: measuredBase,
          };
        }
      }
      if (!isExempt) {
        allEligibleVerified = false;
      }
      return b;
    });
    return { patch: { ...snapshot, branches }, drainVerified: allEligibleVerified };
  }

  async function learnFamily(id: string, path: string, generation: number): Promise<void> {
    if (!familyUnknown.has(id)) return;
    familyUnknown.delete(id);
    let resolved: ResolvedRepo;
    try {
      resolved = await resolvePath(path);
    } catch {
      if (deps.sessionById(id)) familyUnknown.add(id);
      return;
    }
    const session = deps.sessionById(id);
    if (!session || session.generation !== generation) {
      if (session) familyUnknown.add(id);
      return;
    }
    const { commonDir, name, isBare } = resolvedFields(id, resolved);
    deps.applyToSession(id, generation, { commonDir, name, isBare });
  }

  function snapshotIsFresh(id: string, generation: number): boolean {
    const session = deps.sessionById(id);
    const landedAt = sessionBook.hydratedAt.get(id);
    return (
      !!session &&
      session.generation === generation &&
      session.hasHydrated &&
      !session.error &&
      !session.trustRequired &&
      !needsFullPoll(session.watch) &&
      landedAt !== undefined &&
      now() - landedAt < ACTIVATION_FRESH_MS &&
      sessionBook.hydrates.current(id) === null
    );
  }

  async function fetchBranchStats(
    id: string,
    path: string,
    generation: number,
  ): Promise<void> {
    if (statsInflight.has(id)) return;
    const known = deps.sessionById(id);
    if (
      known &&
      known.generation === generation &&
      !known.statsFailed &&
      statsDrained.get(id)?.signature === branchTipSignature(known.branches)
    ) {
      return;
    }
    statsInflight.add(id);
    const liveNow = deps.sessionById(id);
    if (
      liveNow &&
      liveNow.generation === generation &&
      (liveNow.statsFailed ||
        liveNow.branches.some((b) => b.compared_to === undefined))
    ) {
      deps.applyToSession(id, generation, { statsPending: true });
    }
    const start = deps.sessionById(id);
    if (!start || start.generation !== generation) {
      statsInflight.delete(id);
      return;
    }
    const startSignature = branchTipSignature(start.branches);
    const collected = new Map<string, { tip: string; stats: Partial<BranchInfo> }>();
    const withCollectedStats = (list: BranchInfo[]): BranchInfo[] =>
      list.map((branch) => {
        const hit = collected.get(branchStatsKey(branch.name, branch.is_remote, branch.remote_name));
        return hit && hit.tip === branch.tip_commit_id ? { ...branch, ...hit.stats } : branch;
      });
    let dirty = false;
    const flush = (pending: boolean, failed: boolean) => {
      const live = deps.sessionById(id);
      if (
        !dirty &&
        live &&
        live.generation === generation &&
        live.statsPending === pending &&
        live.statsFailed === failed
      ) {
        return;
      }
      if (dirty) {
        deps.applyToSession(id, generation, (current) => ({
          branches: withCollectedStats(current.branches),
          statsPending: pending,
          statsFailed: failed,
        }));
        dirty = false;
        return;
      }
      deps.applyToSession(id, generation, {
        statsPending: pending,
        statsFailed: failed,
      });
    };
    try {
      let lastBatchHadFailures = false;
      let drainedCleanly = false;
      let drainedComparedTo: string | null = null;
      for (let batch = 0; batch < STATS_DRAIN_MAX_BATCHES; batch += 1) {
        const report = await invokeFn<BranchStatsReport>("cmd_branch_stats", {
          repoPath: path,
        });
        const session = deps.sessionById(id);
        if (!session || session.generation !== generation) return;

        lastBatchHadFailures = report.compute_failures > 0;
        if (typeof report.compared_to === "string" && report.compared_to.length > 0) {
          drainedComparedTo = report.compared_to;
        }

        const updates = new Map(
          report.updates.map((update) => [
            branchStatsKey(update.name, update.is_remote, update.remote_name),
            update,
          ]),
        );
        let batchTouched = false;
        for (const [key, update] of updates) {
          collected.set(key, {
            tip: update.tip_commit_id,
            stats: {
              additions: update.additions,
              deletions: update.deletions,
              files_changed: update.files_changed,
              commits_ahead_of_base: update.commits_ahead_of_base,
              commits_behind_base: update.commits_behind_base,
              compared_to: report.compared_to,
            },
          });
        }
        for (const branch of session.branches) {
          const update = updates.get(branchStatsKey(branch.name, branch.is_remote, branch.remote_name));
          if (update && update.tip_commit_id === branch.tip_commit_id) batchTouched = true;
        }
        if (batchTouched) dirty = true;
        const drained = !report.capped;
        if (dirty && (drained || (batch + 1) % STATS_PUBLISH_EVERY === 0)) {
          flush(report.capped, false);
        }
        if (drained) {
          drainedCleanly = true;
          break;
        }
      }
      flush(false, !drainedCleanly || lastBatchHadFailures);
      const settled = deps.sessionById(id);
      if (
        settled &&
        settled.generation === generation &&
        drainedCleanly &&
        !lastBatchHadFailures &&
        branchTipSignature(settled.branches) === startSignature
      ) {
        statsDrained.set(id, {
          signature: startSignature,
          comparedTo: drainedComparedTo,
        });
      } else {
        statsDrained.delete(id);
      }
    } catch {
      statsDrained.delete(id);
      flush(false, true);
    } finally {
      statsInflight.delete(id);
    }
  }

  async function hydrateFacet(
    id: string,
    path: string,
    generation: number,
    facet: HydrationFacet = "all",
  ): Promise<void> {
    if (!deps.sessionById(id)) return;
    const run = (snapshotRuns.get(id) ?? 0) + 1;
    snapshotRuns.set(id, run);
    if (familyUnknown.has(id) && (facet === "all" || facet === "branches")) {
      await learnFamily(id, path, generation);
      if (snapshotRuns.get(id) !== run) return;
    }
    try {
      let patch: Partial<RepoSession> = {};
      let drainVerified = false;
      if (facet === "status") {
        const statusData = await loadFileStatusFacet(path);
        if (snapshotRuns.get(id) !== run) return;
        patch = statusData;
      } else if (facet === "branches") {
        const branchData = await loadBranchGraphFacet(path);
        if (snapshotRuns.get(id) !== run) return;
        const live = deps.sessionById(id);
        if (live) {
          const carried = withCarriedChurn(id, live, branchData);
          patch = carried.patch;
          drainVerified = carried.drainVerified;
        } else {
          patch = branchData;
        }
      } else if (facet === "metadata") {
        const metaData = await loadMetadataFacet(path);
        if (snapshotRuns.get(id) !== run) return;
        patch = metaData;
      } else {
        const raw = await loadSnapshot(path);
        if (snapshotRuns.get(id) !== run) return;
        const live = deps.sessionById(id);
        if (live) {
          const carried = withCarriedChurn(id, live, raw);
          patch = carried.patch;
          drainVerified = carried.drainVerified;
        } else {
          patch = raw;
        }
      }

      deps.applyToSession(id, generation, {
        ...patch,
        isLoading: false,
        error: null,
        trustRequired: false,
        missing: false,
        hasHydrated: true,
        ...(facet === "status" || facet === "all"
          ? { statusPollFailures: 0, statusUnavailableSince: null }
          : {}),
      });

      if (facet === "all" || facet === "branches") {
        const current = deps.sessionById(id);
        if (current?.generation === generation) {
          if (drainVerified && !statsInflight.has(id)) {
            const comparedTo =
              statsDrained.get(id)?.comparedTo ??
              current.branches.find(
                (b) => typeof b.compared_to === "string" && b.compared_to.length > 0,
              )?.compared_to ??
              null;
            statsDrained.set(id, {
              signature: branchTipSignature(current.branches),
              comparedTo,
            });
          }
          if (facet === "all") {
            sessionBook.hydratedAt.set(id, now());
            deps.onContentRevision(path, `${generation}:${run}`);
          }
          void fetchBranchStats(id, path, generation);
        }
      }
    } catch (err: unknown) {
      if (snapshotRuns.get(id) !== run) return;
      const message = describeError(err, "repo");
      deps.applyToSession(id, generation, {
        isLoading: false,
        error: message,
        ...(repositoryTrustRefused(err) ? { trustRequired: true } : {}),
      });
    }
  }

  function hydrate(
    id: string,
    path: string,
    generation: number,
    facet: HydrationFacet = "all",
  ): Promise<void> {
    return sessionBook.hydrates.run(id, generation, () =>
      hydrateFacet(id, path, generation, facet),
    );
  }

  return {
    loadFileStatusFacet,
    loadBranchGraphFacet,
    loadMetadataFacet,
    loadSnapshot,
    resolvePath,
    resolvedFields,
    withCarriedChurn,
    learnFamily,
    snapshotIsFresh,
    fetchBranchStats,
    hydrateFacet,
    hydrate,
  };
}
