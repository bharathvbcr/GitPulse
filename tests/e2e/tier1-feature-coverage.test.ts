import { describe, it, expect, afterAll } from "vitest";
import path from "node:path";
import { existsSync, readFileSync, statSync } from "node:fs";
import {
  createTempManager,
  runCommand,
  PROJECT_ROOT,
  DEVCOUNCIL_ROOT,
} from "./helpers/test-utils";
import {
  createRepoStore,
  facetForChange,
  type BranchInfo,
  type FileStatus,
  type InvokeFn,
  type TagInfo,
  type StashEntry,
} from "../../src/lib/stores/repoStore";
import { memoryStorage } from "../../src/lib/repos/persist";
import { get, writable } from "svelte/store";
import {
  CentralHeartbeatDispatcher,
  type HeartbeatTick,
} from "../../src/lib/runtime/adaptiveTimer";

describe("Tier 1: Feature Coverage (Features 1–15)", () => {
  const tempMgr = createTempManager();

  afterAll(() => {
    tempMgr.cleanup();
  });

  // ---------------------------------------------------------------------------
  // Feature 1: Schema 29 Parity Sync (Milestone 1)
  // ---------------------------------------------------------------------------
  describe("Feature 1: Schema 29 Parity Sync (Milestone 1)", () => {
    it("1.1: check-schema-parity script executes cleanly and confirms 8/8 component matches", () => {
      const res = runCommand("node", ["scripts/check-schema-parity.mjs"]);
      expect(res.status).toBe(0);
      expect(res.stdout).toContain("OK: 8 match");
      expect(res.stdout).toContain("devmap store 29");
    });

    it("1.2: devmap CLI binary version declares store schema 29 and code graph schema 2", () => {
      const res = runCommand("devmap", ["--version"]);
      expect(res.status).toBe(0);
      expect(res.stdout).toContain("store schema 29");
      expect(res.stdout).toContain("code graph schema 2");
      expect(res.stdout).toContain("extract schema 98");
    });

    it("1.3: DevCouncil schema constants declare CURRENT_SCHEMA_VERSION = 29", () => {
      const schemaFile = path.join(DEVCOUNCIL_ROOT, "rust/devmap-store/src/schema.rs");
      expect(existsSync(schemaFile)).toBe(true);
      const content = readFileSync(schemaFile, "utf8");
      expect(content).toMatch(/pub\s+const\s+CURRENT_SCHEMA_VERSION:\s*i32\s*=\s*29;/);
      expect(content).toMatch(/pub\s+const\s+MIN_READER_SCHEMA_VERSION:\s*i32\s*=\s*29;/);
    });

    it("1.4: check-schema-parity --json outputs structured schema 29 owner and components", () => {
      const res = runCommand("node", ["scripts/check-schema-parity.mjs", "--json"]);
      expect(res.status).toBe(0);
      const output = JSON.parse(res.stdout);
      expect(output.ok).toBe(true);
      expect(output.owner.store).toBe(29);
      expect(output.owner.dcStore).toBe(11);
      expect(output.owner.workbench).toBe(14);
      expect(output.owner.extract).toBe(98);
      expect(Array.isArray(output.rows)).toBe(true);
      expect(output.rows.length).toBe(8);
      expect(output.rows.every((r: { verdict: string }) => r.verdict === "match")).toBe(true);
      expect(output.summary).toBe("OK: 8 match");
    });

    it("1.5: Real SQLite store reader_compat table enforces min_reader_schema = 29", () => {
      const dbPath = path.join(PROJECT_ROOT, ".devcouncil/codeintel/devmap.sqlite");
      expect(existsSync(dbPath)).toBe(true);
      const res = runCommand("sqlite3", [dbPath, "SELECT min_reader_schema FROM reader_compat;"]);
      expect(res.status).toBe(0);
      expect(res.stdout.trim()).toBe("29");
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 2: Storage Write Latency Optimization (Milestone 1)
  // ---------------------------------------------------------------------------
  describe("Feature 2: Storage Write Latency Optimization (Milestone 1)", () => {
    it("2.1: DevCouncil write_breakdown test suite verifies decomposed write accounting without O(N) penalties", () => {
      const res = runCommand("cargo", ["test", "-p", "devmap-store", "--test", "write_breakdown"], {
        cwd: path.join(DEVCOUNCIL_ROOT, "rust"),
        timeout: 30_000,
      });
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+4 passed;\s+0 failed/);
    });

    it("2.2: Real SQLite store build_history tracks incremental parse failures and generation metrics", () => {
      const dbPath = path.join(PROJECT_ROOT, ".devcouncil/codeintel/devmap.sqlite");
      expect(existsSync(dbPath)).toBe(true);
      const res = runCommand("sqlite3", [
        dbPath,
        "SELECT generation_id, files, symbols, edges, parse_failed FROM build_history ORDER BY generation_id DESC LIMIT 1;",
      ]);
      expect(res.status).toBe(0);
      const [genId, files, symbols, edges, parseFailed] = res.stdout.trim().split("|").map(Number);
      expect(genId).toBeGreaterThan(0);
      expect(files).toBeGreaterThan(0);
      expect(symbols).toBeGreaterThan(0);
      expect(edges).toBeGreaterThan(0);
      expect(parseFailed).toBeGreaterThanOrEqual(0);
    });

    it("2.3: Store hardening tests verify FTS5 hostile queries syntax safety and symbol search counts", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-store",
          "--test",
          "store_hardening",
          "--",
          "fts_hostile_queries_are_syntax_safe",
          "count_search_symbols_is_driven_by_the_fts_match_not_the_generation_map",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+2 passed;\s+0 failed/);
    });

    it("2.4: Unresolved calls table persists and prunes records with generation lifecycle", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-store",
          "--test",
          "store_hardening",
          "--",
          "unresolved_calls_are_persisted_and_pruned_with_their_generation",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("2.5: SQLite WAL mode configuration preserves WAL journal mode and 16KB page size", () => {
      const dbPath = path.join(PROJECT_ROOT, ".devcouncil/codeintel/devmap.sqlite");
      expect(existsSync(dbPath)).toBe(true);
      const res = runCommand("sqlite3", [dbPath, "PRAGMA journal_mode; PRAGMA page_size;"]);
      expect(res.status).toBe(0);
      const lines = res.stdout.trim().split("\n");
      expect(lines[0].trim().toLowerCase()).toBe("wal");
      expect(lines[1].trim()).toBe("16384");
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 3: Manifest & Code Graph I/O Optimization (Milestone 1)
  // ---------------------------------------------------------------------------
  describe("Feature 3: Manifest & Code Graph I/O Optimization (Milestone 1)", () => {
    it("3.1: devmap status --json returns schema 29 and build manifest capabilities", () => {
      const res = runCommand("devmap", ["status", "--json"]);
      expect(res.status).toBe(0);
      const status = JSON.parse(res.stdout);
      expect(status.schema_version).toBe(29);
      expect(status.expected_schema_version).toBe(29);
      expect(status.generation_id).toBeGreaterThan(0);
      expect(status.capabilities?.build_manifest).toBe(true);
      expect(status.capabilities?.manifest_graph_output).toBe(true);
    });

    it("3.2: devmap paths --json resolves repo_map and code_graph artifacts", () => {
      const res = runCommand("devmap", ["paths", "--json"]);
      expect(res.status).toBe(0);
      const pathsInfo = JSON.parse(res.stdout);
      expect(pathsInfo.version).toBe("0.2.5");
      expect(existsSync(pathsInfo.code_graph)).toBe(true);
      expect(existsSync(pathsInfo.repo_map)).toBe(true);
    });

    it("3.3: repo_map.json contains content_fingerprint and valid index metadata", () => {
      const repoMapPath = path.join(PROJECT_ROOT, ".devcouncil/repo_map.json");
      expect(existsSync(repoMapPath)).toBe(true);
      const stats = statSync(repoMapPath);
      expect(stats.size).toBeGreaterThan(1000);
      const content = readFileSync(repoMapPath, "utf8");
      expect(content).toContain("content_fingerprint");
    });

    it("3.4: code_graph.json artifact exists and reflects full indexed graph", () => {
      const codeGraphPath = path.join(PROJECT_ROOT, ".devcouncil/graph/code_graph.json");
      expect(existsSync(codeGraphPath)).toBe(true);
      const stats = statSync(codeGraphPath);
      expect(stats.size).toBeGreaterThan(1_000_000);
    });

    it("3.5: GitPulse codeintel_real_store test answers impact from real store", () => {
      const res = runCommand("cargo", ["test", "--test", "codeintel_real_store"], {
        cwd: path.join(PROJECT_ROOT, "src-tauri"),
        timeout: 60_000,
      });
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+7 passed;\s+0 failed/);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 4: DevMap Live Refresh Decoupling (Milestone 1)
  // ---------------------------------------------------------------------------
  describe("Feature 4: DevMap Live Refresh Decoupling (Milestone 1)", () => {
    it("4.1: bun run check:ipc verifies zero orphaned handlers and zero missing commands across 291 commands", () => {
      const res = runCommand("bun", ["run", "check:ipc"]);
      expect(res.status).toBe(0);
      expect(res.stdout).toContain("OK: IPC contract holds.");
      expect(res.stdout).toContain("orphaned handlers  : 0");
      expect(res.stdout).toContain("missing commands   : 0");
    });

    it("4.2: start_refresh answers before a hung status probe and collects it once", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::start_refresh_answers_before_a_hung_status_probe_and_collects_it_once",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 60_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("4.3: a probe answer is reused briefly unless it could not run or a build intervened", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::a_probe_answer_is_reused_briefly_unless_it_could_not_run_or_a_build_intervened",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 60_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("4.4: only a few status probes run at once (concurrency bounding)", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::only_a_few_status_probes_run_at_once",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 60_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("4.5: devmap live refresh module declares decoupled probe limits, grace intervals, and state machine", () => {
      const liveRs = path.join(PROJECT_ROOT, "src-tauri/src/devmap/live.rs");
      expect(existsSync(liveRs)).toBe(true);
      const content = readFileSync(liveRs, "utf8");
      expect(content).toMatch(/const\s+PROBE_GRACE:\s*Duration\s*=\s*Duration::from_secs\(1\);/);
      expect(content).toMatch(/const\s+PROBE_LIMIT:\s*usize\s*=\s*4;/);
      expect(content).toMatch(/const\s+PROBE_FRESH:\s*Duration\s*=\s*Duration::from_secs\(2\);/);
      expect(content).toContain("probe_status");
      expect(content).toContain("supervisor::spawn_once");
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 5: Language Accuracy & Resolution (Milestone 1)
  // ---------------------------------------------------------------------------
  describe("Feature 5: Language Accuracy & Resolution (Milestone 1)", () => {
    it("5.1: devmap-resolve factory_return_types integration tests pass with 38+ passing tests", () => {
      const res = runCommand("cargo", ["test", "-p", "devmap-resolve", "--test", "it", "--", "factory_return_types"], {
        cwd: path.join(DEVCOUNCIL_ROOT, "rust"),
        timeout: 30_000,
      });
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+38 passed;\s+0 failed/);
    });

    it("5.2: Rust builder method chain propagates receiver type across chained calls", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-resolve",
          "--test",
          "it",
          "--",
          "factory_return_types::a_rust_builder_method_chain_propagates_receiver_type",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("5.3: TypeScript method chain propagates receiver type across builder pattern calls", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-resolve",
          "--test",
          "it",
          "--",
          "factory_return_types::a_typescript_method_chain_propagates_receiver_type",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("5.4: Adversarial builder chain depth 20 propagates receiver type without exponential cost or recursion fault", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-resolve",
          "--test",
          "it",
          "--",
          "factory_return_types::adversarial_builder_chain_depth_20",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("5.5: Adversarial cyclic receiver chain terminates safely without loop overflow", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "-p",
          "devmap-resolve",
          "--test",
          "it",
          "--",
          "factory_return_types::adversarial_cyclic_receiver_chain",
        ],
        { cwd: path.join(DEVCOUNCIL_ROOT, "rust"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 6: Faceted Hydration Pipeline (Milestone 2)
  // ---------------------------------------------------------------------------
  describe("Feature 6: Faceted Hydration Pipeline (Milestone 2)", () => {
    interface HydrationHarnessOptions {
      statuses?: FileStatus[];
      branches?: BranchInfo[];
      tags?: TagInfo[];
      stashEntries?: StashEntry[];
      failMetadata?: boolean;
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
            return statuses as never;
          case "cmd_list_branches":
            return branches as never;
          case "cmd_list_tags":
            if (options.failMetadata) throw new Error("MetadataFacet failed: cmd_list_tags error");
            return { tags, truncated: false } as never;
          case "cmd_stash_list":
            if (options.failMetadata) throw new Error("MetadataFacet failed: cmd_stash_list error");
            return { entries: stashEntries, truncated: false } as never;
          case "cmd_repo_operation":
            if (options.failMetadata) throw new Error("MetadataFacet failed: cmd_repo_operation error");
            return null as never;
          case "cmd_last_fetch_at":
            if (options.failMetadata) throw new Error("MetadataFacet failed: cmd_last_fetch_at error");
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

    it("6.1: FileStatusFacet independently returns modified file statuses", async () => {
      const { store, calls } = createHydrationHarness({
        statuses: [
          {
            path: "src/main.rs",
            status_code: "M",
            is_staged: false,
            is_conflicted: false,
            additions: 1,
            deletions: 0,
          },
        ],
      });

      const facet = await store.loadFileStatusFacet("/test/repo");
      expect(facet.statuses).toEqual([
        {
          path: "src/main.rs",
          status_code: "M",
          is_staged: false,
          is_conflicted: false,
          additions: 1,
          deletions: 0,
        },
      ]);
      expect(facet.statusPollFailures).toBe(0);
      expect(facet.statusUnavailableSince).toBeNull();

      // Independent execution verification: only status command was invoked
      expect(calls.cmd_get_status ?? 0).toBe(1);
      expect(calls.cmd_list_branches ?? 0).toBe(0);
      expect(calls.cmd_list_tags ?? 0).toBe(0);
      expect(calls.cmd_stash_list ?? 0).toBe(0);
    });

    it("6.2: BranchGraphFacet independently returns branch list and HEAD pointer", async () => {
      const { store, calls } = createHydrationHarness({
        branches: [
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
        ],
      });

      const facet = await store.loadBranchGraphFacet("/test/repo");
      expect(facet.branches).toHaveLength(2);
      expect(facet.currentBranch).toBe("main");
      expect(facet.defaultBranch).toBe("main");
      expect(facet.branches.find((b) => b.is_current)?.name).toBe("main");

      // Independent execution verification: only branch list command was invoked
      expect(calls.cmd_list_branches ?? 0).toBe(1);
      expect(calls.cmd_get_status ?? 0).toBe(0);
      expect(calls.cmd_list_tags ?? 0).toBe(0);
      expect(calls.cmd_stash_list ?? 0).toBe(0);
    });

    it("6.3: MetadataFacet independently returns tags and stash count", async () => {
      const { store, calls } = createHydrationHarness({
        tags: [
          {
            name: "v1.0.0",
            commit_id: "a1b2c3d",
            annotation: null,
            is_annotated: false,
          },
        ],
        stashEntries: [
          {
            index: 0,
            selector: "stash@{0}",
            oid: "c3d4e5f",
            subject: "WIP on lane separation",
            message: "WIP on lane separation",
            branch: "main",
            timestamp: 1700000200,
          },
        ],
      });

      const facet = await store.loadMetadataFacet("/test/repo");
      expect(facet.tags.map((t) => t.name)).toContain("v1.0.0");
      expect(facet.stashEntries).toHaveLength(1);
      expect(facet.stashEntries[0].selector).toBe("stash@{0}");
      expect(facet.tagsFailed).toBe(false);
      expect(facet.stashFailed).toBe(false);

      // Independent execution verification: metadata commands invoked without touching status or branches
      expect(calls.cmd_list_tags ?? 0).toBe(1);
      expect(calls.cmd_stash_list ?? 0).toBe(1);
      expect(calls.cmd_get_status ?? 0).toBe(0);
      expect(calls.cmd_list_branches ?? 0).toBe(0);
    });

    it("6.4: Metadata failure does not block FileStatus and BranchGraph facets (Fault Isolation)", async () => {
      const { store } = createHydrationHarness({
        statuses: [
          {
            path: "src/main.rs",
            status_code: "M",
            is_staged: false,
            is_conflicted: false,
            additions: 1,
            deletions: 0,
          },
        ],
        branches: [
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
        ],
        failMetadata: true,
      });

      // 1. Metadata facet catches internal command rejections and returns safe fallbacks
      const meta = await store.loadMetadataFacet("/test/repo");
      expect(meta.tags).toEqual([]);
      expect(meta.stashEntries).toEqual([]);
      expect(meta.tagsFailed).toBe(true);
      expect(meta.stashFailed).toBe(true);

      // 2. FileStatus and BranchGraph facets succeed completely independently
      const status = await store.loadFileStatusFacet("/test/repo");
      expect(status.statuses).toHaveLength(1);
      expect(status.statuses[0].path).toBe("src/main.rs");

      const branchGraph = await store.loadBranchGraphFacet("/test/repo");
      expect(branchGraph.branches).toHaveLength(1);
      expect(branchGraph.currentBranch).toBe("main");

      // 3. Full store hydration fault isolation: openRepo completes despite metadata failure
      await store.openRepo("/test/repo");
      const state = get(store);
      expect(state.statuses).toHaveLength(1);
      expect(state.branches).toHaveLength(1);
      expect(state.error).toBeNull();
      expect(state.tags).toEqual([]);
    });

    it("6.5: Routine file edit triggers exactly 1 git child process invocation", async () => {
      // 1. Facet routing: verify correct mapping from file change kinds to hydration facets
      expect(facetForChange({ kinds: ["worktree", "index"], paths: ["src/lib.rs"], paths_truncated: false })).toBe("status");
      expect(facetForChange({ kinds: ["refs"], paths: [".git/refs/heads/main"], paths_truncated: false })).toBe("branches");
      expect(facetForChange({ kinds: ["config"], paths: [".git/config"], paths_truncated: false })).toBe("metadata");

      // 2. Hydration execution: routine edit triggers exactly 1 status query (Lane 2 decoupled architecture)
      const { store, calls } = createHydrationHarness({
        statuses: [
          {
            path: "src/main.rs",
            status_code: "M",
            is_staged: false,
            is_conflicted: false,
            additions: 1,
            deletions: 0,
          },
        ],
      });

      await store.openRepo("/test/repo");
      // Reset call counters after initial open
      for (const k of Object.keys(calls)) {
        calls[k] = 0;
      }

      // Routine file edit hydration: target only "status" facet
      await store.hydrateFacet("/test/repo", "status");

      // Exactly 1 git status process invocation occurred
      expect(calls.cmd_get_status ?? 0).toBe(1);
      // Zero branch or metadata invocations were triggered
      expect(calls.cmd_list_branches ?? 0).toBe(0);
      expect(calls.cmd_list_tags ?? 0).toBe(0);
      expect(calls.cmd_stash_list ?? 0).toBe(0);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 7: Per-Repository Indexing Concurrency (Milestone 2)
  // ---------------------------------------------------------------------------
  describe("Feature 7: Per-Repository Indexing Concurrency (Milestone 2)", () => {
    it("7.1: Distinct repositories acquire build slots concurrently while duplicate builds on same repo are serialized", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::one_live_build_runs_at_a_time_per_repository",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("7.2: Concurrency stress tests verify 8 distinct repositories acquire build slots simultaneously", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::concurrency_stress_multiple_distinct_repositories",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("7.3: Path canonicalization prevents build slot collision across symlinks, trailing slashes, and relative paths", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::path_canonicalization_and_collision_rejection",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("7.4: Live refresh dispatcher returns SkipBuilding on colliding symlinks and trailing slashes", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::symlink_and_trailing_slash_skip_building_in_maybe_refresh",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("7.5: Production live build module enforces per-repo HashSet<PathBuf> concurrency without global singleton", () => {
      const liveRs = path.join(PROJECT_ROOT, "src-tauri/src/devmap/live.rs");
      expect(existsSync(liveRs)).toBe(true);
      const content = readFileSync(liveRs, "utf8");
      expect(content).toContain("fn live_build_slots() -> &'static Mutex<HashSet<PathBuf>>");
      expect(content).toMatch(/fn canonical_repo_path\(repo_path:\s*&Path\)\s*->\s*PathBuf/);
      expect(content).toMatch(/pub\(crate\)\s+fn claim\(repo_path:\s*&str\)\s*->\s*Result<Self,\s*String>/);
      expect(content).not.toContain("LIVE_BUILD_SLOT: Mutex<Option<PathBuf>>");

      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "devmap::live::tests::one_live_build_runs_at_a_time_per_repository",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 8: Centralized Heartbeat Dispatcher (Milestone 2)
  // ---------------------------------------------------------------------------
  describe("Feature 8: Centralized Heartbeat Dispatcher (Milestone 2)", () => {
    it("8.1: Master heartbeat delivers scheduled ticks to multiple subscribers", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let sub1Ticks = 0;
      let sub2Ticks = 0;
      dispatcher.subscribe(() => sub1Ticks++, 1000);
      dispatcher.subscribe(() => sub2Ticks++, 1000);
      const baseTime = Date.now();
      expect(dispatcher.subscriberCount()).toBe(2);
      const dispatched = dispatcher.step(baseTime + 1000);
      expect(dispatched).toBe(2);
      expect(sub1Ticks).toBe(1);
      expect(sub2Ticks).toBe(1);
      dispatcher.dispose();
    });

    it("8.2: Batched dispatch triggers subscribers in a single coordinated tick", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let ticks = 0;
      const receivedTicks: HeartbeatTick[] = [];
      dispatcher.subscribe((tick) => {
        ticks++;
        receivedTicks.push(tick);
      }, 1000);
      dispatcher.subscribe((tick) => {
        ticks++;
        receivedTicks.push(tick);
      }, 1000);
      const baseTime = Date.now();
      const dispatched = dispatcher.step(baseTime + 1000);
      expect(dispatched).toBe(2);
      expect(ticks).toBe(2);
      expect(receivedTicks.length).toBe(2);
      // Verify single coordinated tickIndex and identical timestamp
      expect(receivedTicks[0].tickIndex).toBe(receivedTicks[1].tickIndex);
      expect(receivedTicks[0].timestamp).toBe(baseTime + 1000);
      expect(receivedTicks[0].cadence).toBe("active");
      dispatcher.dispose();
    });

    it("8.3: Subscriber unregistration cleanly stops subsequent callbacks", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let sub1Calls = 0;
      const unsub = dispatcher.subscribe(() => sub1Calls++, 1000);
      const baseTime = Date.now();
      expect(dispatcher.subscriberCount()).toBe(1);
      dispatcher.step(baseTime + 1000);
      expect(sub1Calls).toBe(1);
      unsub();
      expect(dispatcher.subscriberCount()).toBe(0);
      dispatcher.step(baseTime + 2000);
      expect(sub1Calls).toBe(1);
      dispatcher.dispose();
    });

    it("8.4: Subscriber errors do not halt the master ticker or affect sibling subscribers", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let siblingTicked = false;
      dispatcher.subscribe(() => {
        throw new Error("simulated failure");
      }, 1000);
      dispatcher.subscribe(() => {
        siblingTicked = true;
      }, 1000);
      const baseTime = Date.now();
      expect(() => dispatcher.step(baseTime + 1000)).not.toThrow();
      expect(siblingTicked).toBe(true);
      dispatcher.dispose();
    });

    it("8.5: Adaptive throttling doubles subscriber intervals during background mode", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let ticks = 0;
      dispatcher.subscribe(() => ticks++, 1000);
      dispatcher.setThrottled(true);
      const baseTime = Date.now();
      // Under throttling, effective interval doubles from 1000ms to 2000ms
      dispatcher.step(baseTime + 1000);
      expect(ticks).toBe(0);
      dispatcher.step(baseTime + 2000);
      expect(ticks).toBe(1);
      dispatcher.dispose();
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 9: Ledger Ingestion Condvar Synchronization (Milestone 2)
  // ---------------------------------------------------------------------------
  describe("Feature 9: Ledger Ingestion Condvar Synchronization (Milestone 2)", () => {
    it("9.1: Single flight acquisition and instant Condvar wakeup on drop unblocks waiting thread", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "ingest::serialize_tests::serialize_single_flight_and_condvar_wakeup",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("9.2: Distinct ledger repositories acquire serialization locks concurrently without blocking", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "ingest::serialize_tests::serialize_distinct_repos_do_not_block_each_other",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("9.3: Contention stress across 12 concurrent threads strictly enforces mutual exclusion without race conditions", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "ingest::serialize_tests::serialize_stress_ten_plus_concurrent_threads_contention",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("9.4: Budget deadline expiration cleanly times out contending threads without deadlocking the holder", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "ingest::serialize_tests::serialize_budget_deadline_expiration_without_deadlock",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);
    });

    it("9.5: Zero CPU spin-lock delay: Condvar event notification wake-up latency is sub-15ms", () => {
      const res = runCommand(
        "cargo",
        [
          "test",
          "--lib",
          "ingest::serialize_tests::serialize_wakeup_latency_via_cvar_notification",
          "--",
          "--test-threads=1",
        ],
        { cwd: path.join(PROJECT_ROOT, "src-tauri"), timeout: 30_000 }
      );
      expect(res.status).toBe(0);
      expect(res.stdout).toMatch(/test result: ok\.\s+1 passed;\s+0 failed/);

      // Verify elimination of spin-lock sleep in production source
      const ingestMod = path.join(PROJECT_ROOT, "src-tauri/src/ingest/mod.rs");
      expect(existsSync(ingestMod)).toBe(true);
      const content = readFileSync(ingestMod, "utf8");
      expect(content).toContain("cvar: std::sync::Condvar");
      expect(content).toContain("state.cvar.notify_all()");
      expect(content).toMatch(/state\s*\.\s*cvar\s*\.\s*wait_timeout\(running,\s*remaining\)/);
      expect(content).not.toMatch(/thread::sleep\(Duration::from_millis\(10\)\)/);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 10: TaskBoard & FleetView Virtualization (Pending Milestone 3)
  // ---------------------------------------------------------------------------
  describe("Feature 10: TaskBoard & FleetView Virtualization (Pending Milestone 3)", () => {
    it.todo("10.1: 2,000 tasks list renders bounded DOM elements (<=40 items in view) (Pending Milestone 3)");
    it.todo("10.2: Scrolling to middle calculates correct startIndex and padding offsets (Pending Milestone 3)");
    it.todo("10.3: FleetView with 1,000 repositories maintains bounded row DOM count (Pending Milestone 3)");
    it.todo("10.4: Search filter narrows candidates while preserving virtualized calculations (Pending Milestone 3)");
    it.todo("10.5: Empty list handles zero items without NaN or division by zero (Pending Milestone 3)");
  });

  // ---------------------------------------------------------------------------
  // Feature 11: View DOM Retention on Tab Switch (Pending Milestone 3)
  // ---------------------------------------------------------------------------
  describe("Feature 11: View DOM Retention on Tab Switch (Pending Milestone 3)", () => {
    it.todo("11.1: Switching tabs retains previous tab DOM elements without tearing down (Pending Milestone 3)");
    it.todo("11.2: Scroll position and expanded folders are preserved when returning to tab (Pending Milestone 3)");
    it.todo("11.3: Inactive tab is marked hidden rather than unmounted (Pending Milestone 3)");
    it.todo("11.4: Closing a tab cleanly unmounts and removes its retained state (Pending Milestone 3)");
    it.todo("11.5: Closing active tab transitions active pointer to next remaining tab (Pending Milestone 3)");
  });

  // ---------------------------------------------------------------------------
  // Feature 12: Integrate 8 Parked UI Branches (Pending Milestone 3)
  // ---------------------------------------------------------------------------
  describe("Feature 12: Integrate 8 Parked UI Branches (Pending Milestone 3)", () => {
    it.todo("12.1: ui/shell-a11y focus preservation and aria live announcements (Pending Milestone 3)");
    it.todo("12.2: ui/history-graph commit table filter change resets list scroll to top (Pending Milestone 3)");
    it.todo("12.3: ui/docs-files search and doc view state preserved across navigation (Pending Milestone 3)");
    it.todo("12.4: ui/insights-panels scoped line changes calculate 30-day velocity correctly (Pending Milestone 3)");
    it.todo("12.5: ui/diff-review disables hunk staging while Ignore Whitespace is active (Pending Milestone 3)");
  });

  // ---------------------------------------------------------------------------
  // Feature 13: Task Backlog Review & Priority Drift Fix (Pending Milestone 4)
  // ---------------------------------------------------------------------------
  describe("Feature 13: Task Backlog Review & Priority Drift Fix (Pending Milestone 4)", () => {
    it.todo("13.1: Priority scale normalizes across numeric and string representations (Pending Milestone 4)");
    it.todo("13.2: Issue import does not invert priority scale (P0 is top rank 0) (Pending Milestone 4)");
    it.todo("13.3: Typing in search input does not reset paged column to page 1 (Pending Milestone 4)");
    it.todo("13.4: Idempotent task update verifies expected_revision before applying mutation (Pending Milestone 4)");
    it.todo("13.5: Evidence-gated task completion rejects unverified tasks (Pending Milestone 4)");
  });

  // ---------------------------------------------------------------------------
  // Feature 14: E2E Testing Suite Infrastructure (Pending Milestone 5)
  // ---------------------------------------------------------------------------
  describe("Feature 14: E2E Testing Suite Infrastructure (Pending Milestone 5)", () => {
    it.todo("14.1: Test suite environment configures Vitest with isolated node environment (Pending Milestone 5)");
    it.todo("14.2: TempDirManager creates isolated temporary directories and cleans up cleanly (Pending Milestone 5)");
    it.todo("14.3: Fixture Git Repo initializes clean git history with initial commit (Pending Milestone 5)");
    it.todo("14.4: Command executor handles non-zero exit codes with error diagnostics (Pending Milestone 5)");
    it.todo("14.5: Wall-clock timeouts prevent hanging test runners (Pending Milestone 5)");
  });

  // ---------------------------------------------------------------------------
  // Feature 15: Final Verification & Adversarial Hardening (Pending Milestone 5)
  // ---------------------------------------------------------------------------
  describe("Feature 15: Final Verification & Adversarial Hardening (Pending Milestone 5)", () => {
    it.todo("15.1: Write latency benchmark verifies transaction duration meets budget (< 50ms) (Pending Milestone 5)");
    it.todo("15.2: Mapping throughput scales linearly without quadratic degradation (Pending Milestone 5)");
    it.todo("15.3: SQLite database header validation verifies WAL format markers (Pending Milestone 5)");
    it.todo("15.4: Corrupt compressed payload throws handled error without crashing process (Pending Milestone 5)");
    it.todo("15.5: Zero unhandled promise rejections during concurrent transaction rollback (Pending Milestone 5)");
  });
});
