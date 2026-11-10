import { get, writable } from "svelte/store";
import {
  createRepoStore,
  facetForChange,
  type BranchInfo,
  type FileStatus,
  type InvokeFn,
  type TagInfo,
  type StashEntry,
} from "../../../src/lib/stores/repoStore";
import { memoryStorage } from "../../../src/lib/repos/persist";

console.log("==================================================================");
console.log("STARTING EMPIRICAL CHALLENGER STRESS HARNESS (Feature 6)");
console.log("==================================================================");

let totalAssertions = 0;
function assert(condition: boolean, msg: string) {
  totalAssertions++;
  if (!condition) {
    console.error(`❌ ASSERTION FAILED: ${msg}`);
    process.exit(1);
  }
}

// ============================================================================
// PART 1: ADVERSARIAL CHALLENGE OF createHydrationHarness & FAULT ISOLATION
// ============================================================================
console.log("\n--- [PART 1] Adversarial Testing of createHydrationHarness ---");

interface HydrationHarnessOptions {
  statuses?: FileStatus[];
  branches?: BranchInfo[];
  tags?: TagInfo[];
  stashEntries?: StashEntry[];
  failTags?: boolean;
  failStash?: boolean;
  failOps?: boolean;
  failFetch?: boolean;
  failStatus?: boolean;
  failBranches?: boolean;
}

function createHydrationHarness(options: HydrationHarnessOptions = {}) {
  const calls: Record<string, number> = {};
  const statuses: FileStatus[] = options.statuses ?? [
    {
      path: "src/main.rs",
      status_code: "M",
      is_staged: false,
      is_conflicted: false,
      additions: 1,
      deletions: 0,
    },
  ];
  const branches: BranchInfo[] = options.branches ?? [
    {
      name: "main",
      is_current: true,
      is_remote: false,
      tip_commit_id: "a1b2c3d",
      ahead_count: 0,
      behind_count: 0,
      is_default: true,
      is_gone: false,
      last_commit_timestamp: 1700000000,
      last_author: "Dev",
      last_summary: "feat: initial commit",
    },
    {
      name: "feature/fast-lane",
      is_current: false,
      is_remote: false,
      tip_commit_id: "e5f6a7b",
      ahead_count: 2,
      behind_count: 0,
      is_default: false,
      is_gone: false,
      last_commit_timestamp: 1700000100,
      last_author: "Dev",
      last_summary: "feat: lane separation",
    },
  ];
  const tags: TagInfo[] = options.tags ?? [
    {
      name: "v1.0.0",
      commit_id: "a1b2c3d",
      annotation: null,
      is_annotated: false,
    },
  ];
  const stashEntries: StashEntry[] = options.stashEntries ?? [
    {
      index: 0,
      selector: "stash@{0}",
      oid: "c3d4e5f",
      subject: "WIP on lane separation",
      message: "WIP on lane separation",
      branch: "main",
      timestamp: 1700000200,
    },
  ];

  const invoke: InvokeFn = async (cmd, args) => {
    calls[cmd] = (calls[cmd] ?? 0) + 1;
    const repoPath = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path: repoPath,
          name: repoPath.split("/").pop() || "repo",
          is_bare: false,
          common_dir: `${repoPath}/.git`,
        } as never;
      case "cmd_get_status":
        if (options.failStatus) throw new Error("cmd_get_status catastrophic failure");
        return statuses as never;
      case "cmd_list_branches":
        if (options.failBranches) throw new Error("cmd_list_branches catastrophic failure");
        return branches as never;
      case "cmd_list_tags":
        if (options.failTags) throw new Error("MetadataFacet failed: cmd_list_tags error");
        return { tags, truncated: false } as never;
      case "cmd_stash_list":
        if (options.failStash) throw new Error("MetadataFacet failed: cmd_stash_list error");
        return { entries: stashEntries, truncated: false } as never;
      case "cmd_repo_operation":
        if (options.failOps) throw new Error("MetadataFacet failed: cmd_repo_operation error");
        return null as never;
      case "cmd_last_fetch_at":
        if (options.failFetch) throw new Error("MetadataFacet failed: cmd_last_fetch_at error");
        return 1700000000000 as never;
      case "cmd_watch_repo":
        return repoPath as never;
      case "cmd_branch_stats":
        return {
          compared_to: "main",
          updates: [],
          computed: 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as never;
      default:
        return undefined as never;
    }
  };

  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe,
      setSearch: () => {},
      selectBranch: () => {},
      clear: () => {},
    },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });

  return { store, calls };
}

async function testHydrationFaultIsolation() {
  // Scenario 1: All metadata commands reject simultaneously
  console.log("  Testing simultaneous failure of all 4 metadata commands...");
  const h1 = createHydrationHarness({
    failTags: true,
    failStash: true,
    failOps: true,
    failFetch: true,
  });

  const meta1 = await h1.store.loadMetadataFacet("/test/repo");
  assert(meta1.tagsFailed === true, "tagsFailed must be true");
  assert(meta1.tags.length === 0, "tags must fallback to empty array");
  assert(meta1.stashFailed === true, "stashFailed must be true");
  assert(meta1.stashEntries.length === 0, "stashEntries must fallback to empty array");
  assert(meta1.operation.probeFailed === true, "probeFailed must be true");
  assert(meta1.fetchedAt === null, "fetchedAt must fallback to null");

  const status1 = await h1.store.loadFileStatusFacet("/test/repo");
  assert(status1.statuses.length === 1, "Status facet unaffected by metadata failure");
  assert(status1.statuses[0].path === "src/main.rs", "Status path matches expected");
  assert(status1.statusPollFailures === 0, "Status poll failures unaffected");

  const branch1 = await h1.store.loadBranchGraphFacet("/test/repo");
  assert(branch1.branches.length === 2, "Branch graph unaffected by metadata failure");
  assert(branch1.currentBranch === "main", "Current branch matches main");

  await h1.store.openRepo("/test/repo");
  const state1 = get(h1.store);
  assert(state1.error === null, "Store openRepo completed with null error despite all metadata commands failing");
  assert(state1.statuses.length === 1, "State statuses intact");
  assert(state1.branches.length === 2, "State branches intact");
  assert(state1.tags.length === 0, "State tags empty fallback");
  console.log("  ✓ All 4 metadata failures isolated cleanly without polluting status or branch data.");

  // Scenario 2: Combinatorial single-command metadata failures
  console.log("  Testing combinatorial isolated metadata command failures...");
  const combinations = [
    { failTags: true, name: "only cmd_list_tags fails" },
    { failStash: true, name: "only cmd_stash_list fails" },
    { failOps: true, name: "only cmd_repo_operation fails" },
    { failFetch: true, name: "only cmd_last_fetch_at fails" },
  ];

  for (const combo of combinations) {
    const h = createHydrationHarness(combo);
    const meta = await h.store.loadMetadataFacet("/test/repo");
    if (combo.failTags) {
      assert(meta.tagsFailed === true && meta.stashFailed === false, "Isolated tag failure isolated from stash");
      assert(meta.tags.length === 0 && meta.stashEntries.length === 1, "Stash succeeded while tags failed");
    }
    if (combo.failStash) {
      assert(meta.stashFailed === true && meta.tagsFailed === false, "Isolated stash failure isolated from tags");
      assert(meta.stashEntries.length === 0 && meta.tags.length === 1, "Tags succeeded while stash failed");
    }
    const status = await h.store.loadFileStatusFacet("/test/repo");
    assert(status.statuses.length === 1, `Status facet unaffected when ${combo.name}`);
    const branch = await h.store.loadBranchGraphFacet("/test/repo");
    assert(branch.branches.length === 2, `Branch facet unaffected when ${combo.name}`);
  }
  console.log("  ✓ Combinatorial metadata failures verified.");

  // Scenario 3: Status command failure does NOT pollute branch or metadata facets
  console.log("  Testing bidirectional isolation: Status failure does not break branches or metadata...");
  const hStatusFail = createHydrationHarness({ failStatus: true });
  let statusErrorCaught = false;
  try {
    await hStatusFail.store.loadFileStatusFacet("/test/repo");
  } catch (err: any) {
    statusErrorCaught = true;
    assert(err.message.includes("cmd_get_status catastrophic failure"), "Expected status error caught");
  }
  assert(statusErrorCaught, "loadFileStatusFacet rejected as expected on status error");

  const branchClean = await hStatusFail.store.loadBranchGraphFacet("/test/repo");
  assert(branchClean.branches.length === 2, "Branch graph succeeded despite status failure");
  const metaClean = await hStatusFail.store.loadMetadataFacet("/test/repo");
  assert(metaClean.tags.length === 1 && metaClean.tagsFailed === false, "Metadata succeeded despite status failure");
  console.log("  ✓ Bidirectional fault isolation verified.");

  // Scenario 4: Exact process invocation counts under Lane 2 decoupled hydration
  console.log("  Testing Lane 2 decoupled child process invocation counts...");
  const hDecoupled = createHydrationHarness();
  await hDecoupled.store.openRepo("/test/repo");
  // Clear call counters
  for (const k of Object.keys(hDecoupled.calls)) {
    hDecoupled.calls[k] = 0;
  }

  // 1. Hydrate status only
  await hDecoupled.store.hydrateFacet("/test/repo", "status");
  assert(hDecoupled.calls.cmd_get_status === 1, "cmd_get_status called exactly once");
  assert(!hDecoupled.calls.cmd_list_branches, "cmd_list_branches NOT called");
  assert(!hDecoupled.calls.cmd_list_tags, "cmd_list_tags NOT called");
  assert(!hDecoupled.calls.cmd_stash_list, "cmd_stash_list NOT called");

  // 2. Hydrate branches only
  for (const k of Object.keys(hDecoupled.calls)) hDecoupled.calls[k] = 0;
  await hDecoupled.store.hydrateFacet("/test/repo", "branches");
  assert(!hDecoupled.calls.cmd_get_status, "cmd_get_status NOT called");
  assert(hDecoupled.calls.cmd_list_branches === 1, "cmd_list_branches called exactly once");
  assert(!hDecoupled.calls.cmd_list_tags, "cmd_list_tags NOT called");
  assert(!hDecoupled.calls.cmd_stash_list, "cmd_stash_list NOT called");

  // 3. Hydrate metadata only
  for (const k of Object.keys(hDecoupled.calls)) hDecoupled.calls[k] = 0;
  await hDecoupled.store.hydrateFacet("/test/repo", "metadata");
  assert(!hDecoupled.calls.cmd_get_status, "cmd_get_status NOT called");
  assert(!hDecoupled.calls.cmd_list_branches, "cmd_list_branches NOT called");
  assert(hDecoupled.calls.cmd_list_tags === 1, "cmd_list_tags called exactly once");
  assert(hDecoupled.calls.cmd_stash_list === 1, "cmd_stash_list called exactly once");
  console.log("  ✓ Decoupled child process invocation counts strictly enforced (1:0:0).");
}

async function main() {
  await testHydrationFaultIsolation();
  console.log("\n==================================================================");
  console.log(`ALL EMPIRICAL CHALLENGES PASSED CLEANLY (${totalAssertions} assertions verified)`);
  console.log("==================================================================");
}

// The stores it builds keep their poll timers alive; without this the script
// printed its verdict and then never exited.
main().then(() => process.exit(0), (err) => {
  console.error("Fatal stress test failure:", err);
  process.exit(1);
});
