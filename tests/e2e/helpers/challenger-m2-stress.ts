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
import {
  CentralHeartbeatDispatcher,
  type HeartbeatTick,
} from "../../../src/lib/runtime/adaptiveTimer";

console.log("==================================================================");
console.log("STARTING EMPIRICAL CHALLENGER STRESS HARNESS (Features 6 & 8)");
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

// ============================================================================
// PART 2: STRESS TESTING CentralHeartbeatDispatcher UNDER HIGH LOAD & RAPID LIFECYCLES
// ============================================================================
console.log("\n--- [PART 2] Stress Testing CentralHeartbeatDispatcher ---");

async function testDispatcherStress() {
  const dispatcher = new CentralHeartbeatDispatcher();

  // Test 1: 10,000 rapid subscriber lifecycle iterations
  console.log("  Running 10,000 rapid subscriber lifecycle iterations (register, step, unsub)...");
  const tStart = performance.now();
  let baseTime = Date.now();
  const unsubs: (() => void)[] = [];
  let tickCount = 0;

  for (let i = 0; i < 10000; i++) {
    const unsub = dispatcher.subscribe(() => {
      tickCount++;
    }, 1000);
    unsubs.push(unsub);

    // Every 50 iterations, step virtual time and unsubscribe half of existing
    if (i % 50 === 0) {
      baseTime += 1000;
      dispatcher.step(baseTime);
      const half = unsubs.splice(0, 25);
      for (const u of half) u();
    }
  }

  // Cleanup all remaining
  for (const u of unsubs) u();
  assert(dispatcher.subscriberCount() === 0, `All subscribers cleaned up, count=${dispatcher.subscriberCount()}`);
  const tElapsed = performance.now() - tStart;
  console.log(`  ✓ 10,000 subscriber lifecycle iterations completed in ${tElapsed.toFixed(2)}ms (subscribers: 0, ticks: ${tickCount}).`);

  // Test 2: 1,000 concurrent subscribers across 4 cadence intervals for 100s virtual time (1,000 steps)
  console.log("  Running 1,000 concurrent subscribers over 100s virtual time (1,000 steps @ 100ms)...");
  const s2 = new CentralHeartbeatDispatcher();
  const intervals = [500, 1000, 2500, 10000];
  const countsPerInterval = 250;
  type SubStat = { interval: number; ticks: number; lastTickIndex: number };
  const stats: SubStat[] = [];

  for (const interval of intervals) {
    for (let i = 0; i < countsPerInterval; i++) {
      const stat: SubStat = { interval, ticks: 0, lastTickIndex: -1 };
      stats.push(stat);
      s2.subscribe((tick: HeartbeatTick) => {
        stat.ticks++;
        stat.lastTickIndex = tick.tickIndex;
      }, interval);
    }
  }
  assert(s2.subscriberCount() === 1000, "1,000 subscribers registered");

  // Step virtual time across 100,000ms in 100ms increments
  let simTime = Date.now();
  let totalDispatched = 0;
  for (let stepIdx = 0; stepIdx < 1000; stepIdx++) {
    simTime += 100;
    const d = s2.step(simTime);
    totalDispatched += d;
  }

  // Verify tick math:
  // 500ms: 100,000 / 500 = 200 ticks
  // 1000ms: 100,000 / 1000 = 100 ticks
  // 2500ms: 100,000 / 2500 = 40 ticks
  // 10000ms: 100,000 / 10000 = 10 ticks
  // Total callbacks = 250*200 + 250*100 + 250*40 + 250*10 = 50000 + 25000 + 10000 + 2500 = 87,500
  for (const stat of stats) {
    if (stat.interval === 500) assert(stat.ticks === 200, `500ms sub has 200 ticks, got ${stat.ticks}`);
    if (stat.interval === 1000) assert(stat.ticks === 100, `1000ms sub has 100 ticks, got ${stat.ticks}`);
    if (stat.interval === 2500) assert(stat.ticks === 40, `2500ms sub has 40 ticks, got ${stat.ticks}`);
    if (stat.interval === 10000) assert(stat.ticks === 10, `10000ms sub has 10 ticks, got ${stat.ticks}`);
  }
  assert(totalDispatched === 87500, `Total callbacks dispatched must be 87,500, got ${totalDispatched}`);
  s2.dispose();
  assert(s2.subscriberCount() === 0, "s2 disposed cleanly");
  console.log(`  ✓ 1,000 subscribers dispatched 87,500 callbacks with 100% mathematical accuracy.`);

  // Test 3: Adversarial error storm (200 crashing subscribers + 800 healthy subscribers)
  console.log("  Running adversarial error storm (200 throwing subscribers + 800 healthy subscribers)...");
  const s3 = new CentralHeartbeatDispatcher();
  let healthyTicks = 0;
  let errorCount = 0;

  for (let i = 0; i < 200; i++) {
    s3.subscribe(() => {
      errorCount++;
      throw new Error(`Adversarial explosion from subscriber ${i}`);
    }, 1000);
  }
  for (let i = 0; i < 800; i++) {
    s3.subscribe(() => {
      healthyTicks++;
    }, 1000);
  }

  let stormTime = Date.now();
  for (let step = 1; step <= 10; step++) {
    stormTime += 1000;
    let thrown = false;
    try {
      const dispatched = s3.step(stormTime);
      assert(dispatched === 1000, `Step ${step}: all 1000 subscribers evaluated`);
    } catch (e) {
      thrown = true;
    }
    assert(!thrown, `Step ${step}: step() must not throw even when 200 subscribers throw`);
  }

  assert(healthyTicks === 8000, `All 800 healthy subscribers executed 10 times (got ${healthyTicks})`);
  assert(errorCount === 2000, `All 200 failing subscribers attempted 10 times (got ${errorCount})`);
  s3.dispose();
  console.log("  ✓ Adversarial error storm fault isolation confirmed: 0 leaked exceptions, 8,000 healthy ticks preserved.");

  // Test 4: Dynamic modification during callback execution (self-unsubscribing)
  console.log("  Running self-unsubscribing dynamic lifecycle during callback...");
  const s4 = new CentralHeartbeatDispatcher();
  let selfUnsubCount = 0;
  const unsubs4: (() => void)[] = [];
  for (let i = 0; i < 100; i++) {
    const unsub = s4.subscribe(() => {
      selfUnsubCount++;
      // Unsubscribe self during callback!
      unsub();
    }, 1000);
    unsubs4.push(unsub);
  }

  assert(s4.subscriberCount() === 100, "100 subscribers before step");
  let t4 = Date.now();
  s4.step(t4 + 1000);
  assert(selfUnsubCount === 100, `All 100 fired and self-unsubscribed (got ${selfUnsubCount})`);
  assert(s4.subscriberCount() === 0, `Subscriber count is 0 after all self-unsubscribed (got ${s4.subscriberCount()})`);

  // Next step should dispatch 0
  const dNext = s4.step(t4 + 2000);
  assert(dNext === 0, "No remaining subscribers dispatched");
  s4.dispose();
  console.log("  ✓ Self-unsubscribing dynamic lifecycle verified.");

  // Test 5: Adaptive Throttling Determinism
  console.log("  Testing adaptive throttling interval doubling determinism...");
  const s5 = new CentralHeartbeatDispatcher();
  let sub1k = 0;
  let sub2k = 0;
  s5.subscribe(() => sub1k++, 1000);
  s5.subscribe(() => sub2k++, 2000);

  let t5 = Date.now();
  // Step 1: normal (1s elapsed)
  s5.step(t5 + 1000);
  assert(sub1k === 1 && sub2k === 0, "Normal step at 1s");

  // Step 2: normal (2s elapsed)
  s5.step(t5 + 2000);
  assert(sub1k === 2 && sub2k === 1, "Normal step at 2s");

  // Enable throttling: effective intervals become 2s (for 1k) and 4s (for 2k)
  s5.setThrottled(true);

  // Step at 3s: 1s elapsed since 2s -> neither fires under throttled mode
  s5.step(t5 + 3000);
  assert(sub1k === 2 && sub2k === 1, "Throttled step at 3s: neither fired");

  // Step at 4s: 2s elapsed since 2s -> 1k fires (interval doubled to 2s), 2k still waiting (needs 4s)
  s5.step(t5 + 4000);
  assert(sub1k === 3 && sub2k === 1, "Throttled step at 4s: 1k fired, 2k waiting");

  // Step at 6s: 4s elapsed since 2s -> both fire
  s5.step(t5 + 6000);
  assert(sub1k === 4 && sub2k === 2, "Throttled step at 6s: both fired");

  // Disable throttling: recovery to 1x
  s5.setThrottled(false);
  s5.step(t5 + 7000);
  assert(sub1k === 5, "Unthrottled recovery step at 7s: 1k fired after 1s");

  s5.dispose();
  console.log("  ✓ Adaptive throttling determinism verified.");
}

async function main() {
  await testHydrationFaultIsolation();
  await testDispatcherStress();
  console.log("\n==================================================================");
  console.log(`ALL EMPIRICAL CHALLENGES PASSED CLEANLY (${totalAssertions} assertions verified)`);
  console.log("==================================================================");
}

main().catch((err) => {
  console.error("Fatal stress test failure:", err);
  process.exit(1);
});
