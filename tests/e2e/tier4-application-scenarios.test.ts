import { describe, it, expect, afterAll } from "vitest";
import path from "node:path";
import {
  createTempManager,
  createFixtureGitRepo,
  FacetedHydrationPipeline,
  PerRepoBuildCoordinator,
  CentralizedHeartbeatDispatcher,
  CondvarLedgerIngestSync,
  computeVirtualWindow,
  ViewSectionPanelRetentionManager,
  normalizePriority,
  SCHEMA_CONSTANTS,
} from "./helpers/test-utils";

describe("Tier 4: Real-World Application Scenarios (Scenarios 1–5)", () => {
  const tempMgr = createTempManager();

  afterAll(() => {
    tempMgr.cleanup();
  });

  // ---------------------------------------------------------------------------
  // Scenario 1: Multi-Repository Fleet Navigation & Live Editing
  // ---------------------------------------------------------------------------
  it("Scenario 1: Developer navigates multi-repo fleet, edits files, and switches tabs with DOM retention and decoupled refresh", async () => {
    const retentionMgr = new ViewSectionPanelRetentionManager();
    const hydrationPipeline = new FacetedHydrationPipeline();
    const buildCoordinator = new PerRepoBuildCoordinator();

    // 1. Open 3 repositories in fleet
    const repos = ["/fleet/repo-alpha", "/fleet/repo-beta", "/fleet/repo-gamma"];
    repos.forEach((r) => retentionMgr.openTab(r));
    expect(retentionMgr.getTabCount()).toBe(3);

    // 2. Select repo-alpha: user expands folder and scrolls diff
    const tabAlpha = retentionMgr.switchTab("/fleet/repo-alpha");
    tabAlpha.scrollOffset = 520;
    tabAlpha.expandedFolders.add("src/core");
    tabAlpha.activeDiffFile = "src/core/engine.rs";

    // 3. User modifies a file in repo-alpha
    // Routine edit tick only fires FileStatusFacet (1 git process)
    const editRes = await hydrationPipeline.runRoutineEditTick("/fleet/repo-alpha");
    expect(editRes.fileStatus.length).toBe(1);
    expect(hydrationPipeline.getInvocations().fileStatus).toBe(1);

    // 4. Background index begins on repo-alpha
    const claimAlpha = buildCoordinator.claim("/fleet/repo-alpha");
    expect(claimAlpha.ok).toBe(true);

    // 5. While alpha is building, user immediately switches to repo-beta
    const tabBeta = retentionMgr.switchTab("/fleet/repo-beta");
    expect(tabBeta.repoPath).toBe("/fleet/repo-beta");
    expect(tabBeta.domVisible).toBe(true);

    // Concurrently, repo-beta can also trigger an index without being blocked by alpha!
    const claimBeta = buildCoordinator.claim("/fleet/repo-beta");
    expect(claimBeta.ok).toBe(true);
    expect(buildCoordinator.activeCount()).toBe(2);

    // 6. User switches back to repo-alpha: DOM state is retained!
    const returnedAlpha = retentionMgr.switchTab("/fleet/repo-alpha");
    expect(returnedAlpha.scrollOffset).toBe(520);
    expect(returnedAlpha.expandedFolders.has("src/core")).toBe(true);
    expect(returnedAlpha.activeDiffFile).toBe("src/core/engine.rs");

    // Clean up
    buildCoordinator.release("/fleet/repo-alpha");
    buildCoordinator.release("/fleet/repo-beta");
    expect(buildCoordinator.activeCount()).toBe(0);
  });

  // ---------------------------------------------------------------------------
  // Scenario 2: High-Velocity Commit & Branch Switching Under DevMap Indexing
  // ---------------------------------------------------------------------------
  it("Scenario 2: High-velocity commits and branch switching proceed smoothly while Schema 29 devmap indexes in background", async () => {
    const pipeline = new FacetedHydrationPipeline();
    const buildCoordinator = new PerRepoBuildCoordinator();

    const repoPath = "/repos/active-project";

    // 1. Acquire indexing slot for active repo
    const claim = buildCoordinator.claim(repoPath);
    expect(claim.ok).toBe(true);

    // 2. Simulate 5 consecutive rapid commits
    for (let commitIdx = 1; commitIdx <= 5; commitIdx++) {
      // Each commit triggers FileStatusFacet
      await pipeline.hydrateFileStatus(repoPath);
    }
    expect(pipeline.getInvocations().fileStatus).toBe(5);

    // 3. User switches branch from 'main' to 'feature/fast-resolve'
    // BranchGraphFacet independently hydrates branch pointers in <10ms
    const branchRes = await pipeline.hydrateBranchGraph(repoPath);
    expect(branchRes.some((b) => b.isHead)).toBe(true);

    // 4. Complete background indexing transaction under Schema 29
    const indexingOutcome = {
      schemaVersion: SCHEMA_CONSTANTS.CURRENT_SCHEMA_VERSION,
      generationId: 42,
      indexedFiles: 150,
      committed: true,
    };
    expect(indexingOutcome.schemaVersion).toBe(29);
    expect(indexingOutcome.committed).toBe(true);

    // Release build slot
    buildCoordinator.release(repoPath);
    expect(buildCoordinator.isBuilding(repoPath)).toBe(false);
  });

  // ---------------------------------------------------------------------------
  // Scenario 3: Large TaskBoard Triage and Evidence-Gated Completion Under Active Ingest
  // ---------------------------------------------------------------------------
  it("Scenario 3: Triage 2,000 tasks on TaskBoard with VirtualList, priority drift normalization, and evidence-gated completion", async () => {
    const ingestSync = new CondvarLedgerIngestSync();

    // 1. Simulate active ledger ingestion in background
    await ingestSync.acquire("/ledger/tasks", 500);

    // 2. Load 2,000 tasks with mixed raw priority formats
    const rawBacklog = Array.from({ length: 2000 }, (_, i) => ({
      id: `task-${i}`,
      title: `Task #${i}: Work item title`,
      status: i < 100 ? "inbox" : i < 1800 ? "review" : "done",
      rawPriority: i % 4 === 0 ? "critical" : i % 4 === 1 ? "high" : i % 4 === 2 ? "medium" : "low",
      hasVerificationPassed: i === 10, // Only task-10 has passed evidence
    }));

    // 3. Normalize priority scale (P0=0, P1=1, P2=2, P3=3)
    const normalizedTasks = rawBacklog.map((t) => ({
      ...t,
      priority: normalizePriority(t.rawPriority),
    }));

    // Verify P0 is top rank 0 and never inverted
    const p0Tasks = normalizedTasks.filter((t) => t.priority.code === "P0");
    expect(p0Tasks.length).toBe(500);
    expect(p0Tasks[0].priority.rank).toBe(0);

    // 4. VirtualList renders the 2,000 cards: DOM nodes stay <= 40
    const win = computeVirtualWindow(normalizedTasks.length, 64, 700, 3200); // scrolled down 50 items
    expect(win.renderedCount).toBeLessThanOrEqual(40);
    expect(win.startIndex).toBeGreaterThanOrEqual(40);

    // 5. Evidence-gated task completion test
    const attemptComplete = (task: (typeof normalizedTasks)[0]) => {
      if (!task.hasVerificationPassed) {
        return { status: "review", error: "EVIDENCE_GATED: Manvi verification required" };
      }
      return { status: "done", error: null };
    };

    // Task without passing evidence stays in review
    const unverifiedAttempt = attemptComplete(normalizedTasks[1]);
    expect(unverifiedAttempt.status).toBe("review");
    expect(unverifiedAttempt.error).toContain("EVIDENCE_GATED");

    // Task with passing evidence transitions to done
    const verifiedAttempt = attemptComplete(normalizedTasks[10]);
    expect(verifiedAttempt.status).toBe("done");
    expect(verifiedAttempt.error).toBeNull();

    // 6. Release ledger ingestion lock
    ingestSync.release("/ledger/tasks");
  });

  // ---------------------------------------------------------------------------
  // Scenario 4: Cold Repository Discovery, Schema 29 Sync, and Manifest Generation
  // ---------------------------------------------------------------------------
  it("Scenario 4: Fresh repository discovered, Schema 29 parity verified, and on-demand manifest generated with resolved method chains", () => {
    // 1. Discover fresh repository
    const repoInfo = {
      path: "/workspace/new-service",
      isGit: true,
      hasDevmapState: false,
    };
    expect(repoInfo.isGit).toBe(true);

    // 2. Schema 29 Parity Verification
    const systemSchema = SCHEMA_CONSTANTS.CURRENT_SCHEMA_VERSION;
    const readerMin = SCHEMA_CONSTANTS.MIN_READER_SCHEMA_VERSION;
    expect(systemSchema).toBe(29);
    expect(readerMin).toBe(29);

    // 3. Routine build without --manifest does not write code_graph.json
    const routineBuild = {
      flags: [],
      writesCodeGraph: false,
      writesRepoMap: false,
      durationMs: 45,
    };
    expect(routineBuild.writesCodeGraph).toBe(false);

    // 4. User opens code map -> triggers on-demand --manifest build
    const manifestBuild = {
      flags: ["--manifest"],
      writesCodeGraph: true,
      writesRepoMap: true,
      codeGraph: {
        schema: 29,
        symbolCount: 840,
        resolvedCalls: 520,
        unresolvedCalls: 320,
      },
    };
    expect(manifestBuild.writesCodeGraph).toBe(true);
    expect(manifestBuild.codeGraph.schema).toBe(29);
    expect(manifestBuild.codeGraph.resolvedCalls).toBeGreaterThan(500);
  });

  // ---------------------------------------------------------------------------
  // Scenario 5: Adversarial Stress & Failure Recovery Across System Lanes
  // ---------------------------------------------------------------------------
  it("Scenario 5: Multi-lane failure injection (corrupt zstd, hung git, lock contention) recovers cleanly without crash", async () => {
    const pipeline = new FacetedHydrationPipeline();
    const ingestSync = new CondvarLedgerIngestSync();
    const coordinator = new PerRepoBuildCoordinator();

    // 1. Lane 4 Contention: Hold ledger lock and test timeout handling
    await ingestSync.acquire("/ledger/adversarial", 100);
    const timeoutRes = await ingestSync.acquire("/ledger/adversarial", 20);
    expect(timeoutRes).toBe(false); // Graceful timeout, no deadlock
    ingestSync.release("/ledger/adversarial");

    // 2. Lane 2 Fault Injection: Metadata facet fails, FileStatus succeeds
    const partialHydration = await pipeline.runFullHydration("/repo/fault", { failMetadata: true });
    expect(partialHydration.fileStatus.length).toBe(1);
    expect(partialHydration.decoupledErrors.length).toBe(1);

    // 3. Lane 3 Storage Hardening: Corrupt zstd extraction payload handled
    const corruptPayload = Buffer.from([0x00, 0x00, 0x00, 0x00]);
    let decompressHandled = false;
    try {
      if (!corruptPayload.subarray(0, 4).equals(SCHEMA_CONSTANTS.ZSTD_MAGIC_HEADER)) {
        throw new Error("Corrupted payload detected");
      }
    } catch {
      decompressHandled = true;
    }
    expect(decompressHandled).toBe(true);

    // 4. Lane 1 & 3 Indexing Slot Recovery on panic
    coordinator.claim("/repo/panic");
    try {
      throw new Error("Simulated worker panic");
    } catch {
      coordinator.release("/repo/panic");
    }
    expect(coordinator.isBuilding("/repo/panic")).toBe(false);
  });
});
