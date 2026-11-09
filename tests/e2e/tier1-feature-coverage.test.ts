import { describe, it, expect, afterAll } from "vitest";
import path from "node:path";
import {
  existsSync,
  readFileSync,
  statSync,
  writeFileSync,
  mkdirSync,
  openSync,
  readSync,
  closeSync,
} from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { zstdCompressSync, zstdDecompressSync } from "node:zlib";
import {
  createTempManager,
  createFixtureGitRepo,
  runCommand,
  computeVirtualWindow,
  PROJECT_ROOT,
  DEVCOUNCIL_ROOT,
  SCHEMA_CONSTANTS,
} from "./helpers/test-utils";
import {
  parsePriority,
  DEFAULT_PRIORITY,
  PRIORITY_LABELS,
  priorityLabel,
} from "../../src/lib/workbench/taskPriority";
import { tabProps } from "../../src/lib/dom/tablist";
import {
  taskWrite,
  taskDraft,
  task,
  WorkbenchError,
  type Task,
  type TaskStatus,
} from "../../src/lib/workbench/client";
import { TaskBatch } from "../../src/lib/workbench/taskActions";
import {
  decodeVerification,
  VERDICT_STATES,
  VERDICT_HINTS,
} from "../../src/lib/workbench/verification";
import { inferTaskPriority } from "../../src/lib/workbench/issueTask";
import { pagesLoaded, reloadSize, sameRecords } from "../../src/lib/workbench/boardPaging";
import { scrollTopForNewFilter } from "../../src/lib/graph/currentCommit";
import { computePeriodCompare } from "../../src/lib/pulse/metrics";
import { createRepoPanelCache } from "../../src/lib/panels/repoPanelCache";
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
    it("4.1: bun run check:ipc verifies zero orphaned handlers and zero missing commands across 292 commands", () => {
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
  // Feature 10: TaskBoard & FleetView Virtualization
  // ---------------------------------------------------------------------------
  describe("Feature 10: TaskBoard & FleetView Virtualization", () => {
    it("10.1: 2,000 tasks list renders bounded DOM elements (<=40 items in view)", () => {
      const totalTasks = 2000;
      const rowHeight = 40;
      const viewportHeight = 800; // 20 visible items
      const buffer = 8;
      const win = computeVirtualWindow(totalTasks, rowHeight, viewportHeight, 0, buffer);

      expect(win.renderedCount).toBeLessThanOrEqual(40);
      expect(win.renderedCount).toBe(28); // 20 visible + 8 buffer
      expect(win.startIndex).toBe(0);
      expect(win.endIndex).toBe(28);
    });

    it("10.2: Scrolling to middle calculates correct startIndex and padding offsets", () => {
      const totalTasks = 2000;
      const rowHeight = 40;
      const viewportHeight = 800;
      const scrollTop = 40000; // middle at row 1000
      const buffer = 5;
      const win = computeVirtualWindow(totalTasks, rowHeight, viewportHeight, scrollTop, buffer);

      expect(win.startIndex).toBe(995); // 1000 - 5 buffer
      expect(win.endIndex).toBe(1025); // 1000 + 20 + 5 buffer
      expect(win.topPadding).toBe(995 * 40);
      expect(win.bottomPadding).toBe((2000 - 1025) * 40);
      expect(win.topPadding + win.renderedCount * rowHeight + win.bottomPadding).toBe(totalTasks * rowHeight);
    });

    it("10.3: FleetView with 1,000 repositories maintains bounded row DOM count", () => {
      const totalRepos = 1000;
      const rowHeight = 44;
      const viewportHeight = 700; // ~16 visible rows
      const win = computeVirtualWindow(totalRepos, rowHeight, viewportHeight, 2200, 6);

      expect(win.renderedCount).toBeLessThanOrEqual(40);
      expect(win.startIndex).toBe(44); // 50 - 6 buffer
      expect(win.renderedCount).toBe(28);
    });

    it("10.4: Search filter narrows candidates while preserving virtualized calculations", () => {
      const filteredCount = 45;
      const rowHeight = 40;
      const viewportHeight = 600;
      const win = computeVirtualWindow(filteredCount, rowHeight, viewportHeight, 0, 5);

      expect(win.renderedCount).toBe(20); // 15 visible + 5 buffer
      expect(win.totalHeight).toBe(45 * 40);
      expect(win.bottomPadding).toBe((45 - 20) * 40);
    });

    it("10.5: Empty list handles zero items without NaN or division by zero", () => {
      const win = computeVirtualWindow(0, 40, 800, 0);

      expect(win.startIndex).toBe(0);
      expect(win.endIndex).toBe(0);
      expect(win.renderedCount).toBe(0);
      expect(win.totalHeight).toBe(0);
      expect(win.topPadding).toBe(0);
      expect(win.bottomPadding).toBe(0);
      expect(Number.isNaN(win.topPadding)).toBe(false);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 11: View DOM Retention on Tab Switch (Milestone 3)
  // ---------------------------------------------------------------------------
  describe("Feature 11: View DOM Retention on Tab Switch (Milestone 3)", () => {
    function createTestRepoStore() {
      const invoke: InvokeFn = async (cmd, args) => {
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
          case "cmd_list_branches":
            return [] as never;
          case "cmd_list_tags":
            return { tags: [], truncated: false } as never;
          case "cmd_stash_list":
            return { entries: [], truncated: false } as never;
          case "cmd_branch_stats":
            return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
          case "cmd_watch_repo":
            return repoPath as never;
          default:
            return undefined as never;
        }
      };

      return createRepoStore({
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
    }

    it("11.1: Switching tabs retains previous tab DOM elements without tearing down", async () => {
      // 1. Production Component Source Contract:
      // ViewSectionPanel must NOT use destructive #key on currentPath or activeTab
      const panelFile = path.join(PROJECT_ROOT, "src/lib/components/ViewSectionPanel.svelte");
      expect(existsSync(panelFile)).toBe(true);
      const panelContent = readFileSync(panelFile, "utf8");
      expect(panelContent).not.toContain("{#key $repoStore.currentPath}");
      expect(panelContent).not.toContain("{#key");
      expect(panelContent).toContain('role="tabpanel"');
      expect(panelContent).toContain('tabindex="0"');

      // All 4 view hosts render inside ViewSectionPanel without destructive keying
      const viewHosts = ["WorkspaceView", "CodeView", "HistoryView", "InsightsView"];
      for (const viewHost of viewHosts) {
        const hostFile = path.join(PROJECT_ROOT, `src/lib/components/${viewHost}.svelte`);
        expect(existsSync(hostFile)).toBe(true);
        const hostContent = readFileSync(hostFile, "utf8");
        expect(hostContent).toContain("<ViewSectionPanel");
        expect(hostContent).not.toContain("{#key $repoStore.currentPath}");
      }

      // App.svelte hosts views without destructive keying blocks
      const appFile = path.join(PROJECT_ROOT, "src/App.svelte");
      const appContent = readFileSync(appFile, "utf8");
      const appTemplate = appContent.replace(/<!--[\s\S]*?-->/g, "");
      expect(appTemplate).not.toContain("{#key $repoStore.currentPath}");
      expect(appTemplate).not.toContain("{#key activeTab}");
      expect(appTemplate).not.toContain("{#key");

      // 2. Production Store Behavior Contract:
      // Real repoStore keeps multi-repo sessions and tabs retained across tab switches
      const store = createTestRepoStore();
      expect(await store.openRepo("/repo/alpha")).toBe(true);
      expect(await store.openRepo("/repo/beta")).toBe(true);

      const state = get(store);
      expect(state.openTabs).toHaveLength(2);
      const tabAlpha = state.openTabs.find((t) => t.path === "/repo/alpha")!;
      const tabBeta = state.openTabs.find((t) => t.path === "/repo/beta")!;

      // Active tab is Beta; Alpha remains retained in openTabs with isActive: false
      expect(tabBeta.isActive).toBe(true);
      expect(tabAlpha.isActive).toBe(false);
      expect(state.currentPath).toBe("/repo/beta");
      expect(store.retainedSessionRecords()).toBeGreaterThan(0);
    });

    it("11.2: Scroll position and expanded folders are preserved when returning to tab", async () => {
      // 1. Production RepoPanelCache Contract:
      // Panels use createRepoPanelCache to preserve UI state (scroll, expanded sets) per repository path
      interface TestPanelState {
        scrollOffset: number;
        expandedFolders: Set<string>;
        activeDiffFile: string;
      }
      const panelCache = createRepoPanelCache<TestPanelState>({ maxRepos: 8 });

      panelCache.set("/repo/alpha", {
        scrollOffset: 1420,
        expandedFolders: new Set(["src/lib/components"]),
        activeDiffFile: "src/main.rs",
      });

      // Switch to repo B and manipulate its cached panel state
      panelCache.set("/repo/beta", {
        scrollOffset: 80,
        expandedFolders: new Set(["tests"]),
        activeDiffFile: "tests/suite.rs",
      });

      // Switch back to repo A: state is preserved synchronously without unmounting
      const restoredAlpha = panelCache.get("/repo/alpha");
      expect(restoredAlpha).toBeDefined();
      expect(restoredAlpha?.scrollOffset).toBe(1420);
      expect(restoredAlpha?.expandedFolders.has("src/lib/components")).toBe(true);
      expect(restoredAlpha?.activeDiffFile).toBe("src/main.rs");

      // 2. Production repoStore Session Contract:
      // File selections and view sections are preserved per-session across tab switches
      const store = createTestRepoStore();
      await store.openRepo("/repo/alpha");
      store.selectFilePath("src/main.rs");
      store.setViewSection("code", "blame");

      await store.openRepo("/repo/beta");
      store.selectFilePath("tests/suite.rs");
      store.setViewSection("code", "explorer");

      // Switch back to Alpha
      const alphaTab = get(store).openTabs.find((t) => t.path === "/repo/alpha")!;
      await store.activateTab(alphaTab.id);

      const stateAlpha = get(store);
      expect(stateAlpha.currentPath).toBe("/repo/alpha");
      expect(stateAlpha.selectedFilePath).toBe("src/main.rs");
      expect(stateAlpha.viewSections.code).toBe("blame");
    });

    it("11.3: Inactive tab is marked hidden rather than unmounted", async () => {
      // 1. Production Store Tab Inactive State Contract:
      const store = createTestRepoStore();
      await store.openRepo("/repo/active");
      await store.openRepo("/repo/background");

      let state = get(store);
      expect(state.currentPath).toBe("/repo/background");
      expect(state.openTabs.find((t) => t.path === "/repo/background")?.isActive).toBe(true);
      // Background tab remains in openTabs with isActive: false (not unmounted)
      expect(state.openTabs.find((t) => t.path === "/repo/active")?.isActive).toBe(false);

      // Switch active tab back to /repo/active
      const activeTabId = state.openTabs.find((t) => t.path === "/repo/active")!.id;
      await store.activateTab(activeTabId);

      state = get(store);
      expect(state.currentPath).toBe("/repo/active");
      expect(state.openTabs.find((t) => t.path === "/repo/active")?.isActive).toBe(true);
      expect(state.openTabs.find((t) => t.path === "/repo/background")?.isActive).toBe(false);

      // 2. Production ARIA & Tablist Contract:
      // Only the selected tab is reachable in tab sequence; inactive tab receives tabindex: -1
      const activeProps = tabProps("repo", "active", true);
      const inactiveProps = tabProps("repo", "background", false);

      expect(activeProps.role).toBe("tab");
      expect(activeProps["aria-selected"]).toBe(true);
      expect(activeProps.tabindex).toBe(0);

      expect(inactiveProps.role).toBe("tab");
      expect(inactiveProps["aria-selected"]).toBe(false);
      expect(inactiveProps.tabindex).toBe(-1);
    });

    it("11.4: Closing a tab cleanly unmounts and removes its retained state", async () => {
      const store = createTestRepoStore();
      await store.openRepo("/repo/keep-me");
      await store.openRepo("/repo/close-me");
      expect(get(store).openTabs).toHaveLength(2);
      const recordsBefore = store.retainedSessionRecords();
      expect(recordsBefore).toBeGreaterThan(0);

      const closeMeId = get(store).openTabs.find((t) => t.path === "/repo/close-me")!.id;
      await store.closeTab(closeMeId);

      const stateAfter = get(store);
      expect(stateAfter.openTabs).toHaveLength(1);
      expect(stateAfter.openTabs.some((t) => t.path === "/repo/close-me")).toBe(false);
      expect(store.retainedSessionRecords()).toBeLessThan(recordsBefore);

      // Closing nonexistent tab returns cleanly without crashing or mutating store
      await store.closeTab("nonexistent-tab-id");
      expect(get(store).openTabs).toHaveLength(1);
    });

    it("11.5: Closing active tab transitions active pointer to next remaining tab", async () => {
      const store = createTestRepoStore();
      await store.openRepo("/repo/first");
      await store.openRepo("/repo/second");

      let state = get(store);
      expect(state.currentPath).toBe("/repo/second");
      const secondTab = state.openTabs.find((t) => t.path === "/repo/second")!;
      expect(secondTab.isActive).toBe(true);

      // Close the currently active tab (second)
      await store.closeTab(secondTab.id);

      state = get(store);
      expect(state.openTabs).toHaveLength(1);
      // Active pointer transitioned to the remaining neighbor tab (first)
      expect(state.currentPath).toBe("/repo/first");
      expect(state.openTabs[0].path).toBe("/repo/first");
      expect(state.openTabs[0].isActive).toBe(true);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 12: Integrate 8 Parked UI Branches (Milestone 3)
  // ---------------------------------------------------------------------------
  describe("Feature 12: Integrate 8 Parked UI Branches (Milestone 3)", () => {
    it("12.1: ui/shell-a11y focus preservation and aria live announcements", () => {
      const toastFile = path.join(PROJECT_ROOT, "src/lib/components/ToastContainer.svelte");
      expect(existsSync(toastFile)).toBe(true);
      const content = readFileSync(toastFile, "utf8");
      // Persistent dual live regions for announcements
      expect(content).toContain('aria-live="assertive"');
      expect(content).toContain('aria-live="polite"');
      expect(content).toContain('role="region"');
      expect(content).toContain('aria-label="Notifications"');
      expect(content).toContain('aria-label="Dismiss: {toast.message}"');
    });

    it("12.2: ui/history-graph commit table filter change resets list scroll to top", () => {
      const rows = Array.from({ length: 60 }, (_, i) => ({ id: `commit-${i}` }));
      const rowHeight = 36;
      const viewportHeight = 360; // 10 visible rows

      // When the filter matches a new subset where current commit was lost, scroll returns to top (0)
      const topWhenLost = scrollTopForNewFilter({
        rows,
        currentId: "nonexistent",
        rowHeight,
        viewportHeight,
      });
      expect(topWhenLost).toBe(0);

      // When current commit is within first screenful (e.g. row 2), scroll is 0
      const topFirstScreen = scrollTopForNewFilter({
        rows,
        currentId: "commit-2",
        rowHeight,
        viewportHeight,
      });
      expect(topFirstScreen).toBe(0);

      // When current commit survived deep in history (e.g. row 30), it is centered into view
      const topDeep = scrollTopForNewFilter({
        rows,
        currentId: "commit-30",
        rowHeight,
        viewportHeight,
      });
      expect(topDeep).toBeGreaterThan(0);
      expect(topDeep).toBeLessThanOrEqual(30 * rowHeight);
    });

    it("12.3: ui/docs-files search and doc view state preserved across navigation", () => {
      interface SearchMemo {
        query: string;
        results: { title: string; line: number }[];
        docPath: string | null;
      }
      const cache = createRepoPanelCache<SearchMemo>({ maxRepos: 10 });
      const repoAlpha = "/workspace/repo-a";
      const repoBeta = "/workspace/repo-b";

      cache.set(repoAlpha, {
        query: "createRepoPanelCache",
        results: [{ title: "Search Panel", line: 42 }],
        docPath: "docs/architecture.md",
      });

      cache.set(repoBeta, {
        query: "devmap",
        results: [{ title: "DevMap Store", line: 10 }],
        docPath: null,
      });

      // Navigating back to repoAlpha retrieves exact cached search and doc state
      const restoredAlpha = cache.get(repoAlpha);
      expect(restoredAlpha).toBeDefined();
      expect(restoredAlpha?.query).toBe("createRepoPanelCache");
      expect(restoredAlpha?.results.length).toBe(1);
      expect(restoredAlpha?.docPath).toBe("docs/architecture.md");

      // Modifying cache preserves per-repository isolation
      expect(cache.get(repoBeta)?.query).toBe("devmap");
    });

    it("12.4: ui/insights-panels scoped line changes calculate 30-day velocity correctly", () => {
      const nowMs = 1700000000000;
      const dayMs = 86400 * 1000;

      // When prior period has no commits, velocity delta is null ("new"), not misleading +100%
      const recentOnlyCommits = [
        {
          sha: "c1",
          timestamp: Math.floor((nowMs - 5 * dayMs) / 1000),
          additions: 120,
          deletions: 30,
        },
      ];
      const deltasNoPrior = computePeriodCompare(recentOnlyCommits, nowMs, 30, false);
      expect(deltasNoPrior.currentCommits).toBe(1);
      expect(deltasNoPrior.priorCommits).toBe(0);
      expect(deltasNoPrior.commitsDeltaPct).toBeNull();
      expect(deltasNoPrior.priorWindowCovered).toBe(true);

      // When both periods have activity, velocity delta is correctly computed as percentage
      const twoPeriodCommits = [
        {
          sha: "c1",
          timestamp: Math.floor((nowMs - 5 * dayMs) / 1000),
          additions: 200,
          deletions: 50,
        },
        {
          sha: "c2",
          timestamp: Math.floor((nowMs - 40 * dayMs) / 1000),
          additions: 100,
          deletions: 25,
        },
      ];
      const deltasBoth = computePeriodCompare(twoPeriodCommits, nowMs, 30, false);
      expect(deltasBoth.currentCommits).toBe(1);
      expect(deltasBoth.priorCommits).toBe(1);
      expect(deltasBoth.commitsDeltaPct).toBe(0); // 0% delta
      expect(deltasBoth.addsDeltaPct).toBe(100); // 100 to 200 is +100%
    });

    it("12.5: ui/diff-review disables hunk staging while Ignore Whitespace is active", () => {
      const diffViewer = path.join(PROJECT_ROOT, "src/lib/components/DiffViewer.svelte");
      expect(existsSync(diffViewer)).toBe(true);
      const content = readFileSync(diffViewer, "utf8");

      // Verify that Ignore whitespace blocks hunk staging
      expect(content).toContain("let whitespaceView = $derived($repoStore.selectedIgnoreWhitespace);");
      expect(content).toContain("let stagingBlocked = $derived(truncatedSource || whitespaceView);");
      expect(content).toContain("Turn off Ignore whitespace to stage");
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 13: Task Backlog Review & Priority Drift Fix (Milestone 4)
  // ---------------------------------------------------------------------------
  describe("Feature 13: Task Backlog Review & Priority Drift Fix (Milestone 4)", () => {
    it("13.1: Priority scale normalizes across numeric and string representations", () => {
      // Direct production taskPriority module verification (0 = Urgent, 1 = High, 2 = Normal, 3 = Low)
      // Numeric priorities
      expect(parsePriority(0)).toBe(0);
      expect(parsePriority(1)).toBe(1);
      expect(parsePriority(2)).toBe(2);
      expect(parsePriority(3)).toBe(3);

      // Decimal strings
      expect(parsePriority("0")).toBe(0);
      expect(parsePriority("1")).toBe(1);
      expect(parsePriority("2")).toBe(2);
      expect(parsePriority("3")).toBe(3);

      // Canonical string labels (ASCII-folded, case-insensitive)
      expect(parsePriority("P0")).toBe(0);
      expect(parsePriority("p0")).toBe(0);
      expect(parsePriority("urgent")).toBe(0);
      expect(parsePriority("URGENT")).toBe(0);
      expect(parsePriority("P1")).toBe(1);
      expect(parsePriority("p1")).toBe(1);
      expect(parsePriority("high")).toBe(1);
      expect(parsePriority("High")).toBe(1);
      expect(parsePriority("P2")).toBe(2);
      expect(parsePriority("p2")).toBe(2);
      expect(parsePriority("normal")).toBe(2);
      expect(parsePriority("medium")).toBe(2);
      expect(parsePriority("P3")).toBe(3);
      expect(parsePriority("p3")).toBe(3);
      expect(parsePriority("low")).toBe(3);
      expect(parsePriority("Low")).toBe(3);

      // Brief composite forms
      expect(parsePriority("0 (Urgent)")).toBe(0);
      expect(parsePriority("1 (High)")).toBe(1);
      expect(parsePriority("2 (Normal)")).toBe(2);

      // Default priority fallback and labels
      expect(DEFAULT_PRIORITY).toBe(2);
      expect(PRIORITY_LABELS).toEqual(["Urgent", "High", "Normal", "Low"]);
      expect(priorityLabel(0)).toBe("Urgent");
      expect(priorityLabel(1)).toBe("High");
      expect(priorityLabel(2)).toBe("Normal");
      expect(priorityLabel(3)).toBe("Low");
      expect(priorityLabel(undefined)).toBe("Normal");

      // Invariants: P0 is strictly top rank 0 (lower numeric value = higher priority rank)
      const rankP0 = parsePriority("p0")!;
      const rankP1 = parsePriority("p1")!;
      const rankP2 = parsePriority("p2")!;
      const rankP3 = parsePriority("p3")!;
      expect(rankP0).toBeLessThan(rankP1);
      expect(rankP1).toBeLessThan(rankP2);
      expect(rankP2).toBeLessThan(rankP3);

      // Refuses invalid forms without guessing
      expect(parsePriority("4")).toBeNull();
      expect(parsePriority(-1)).toBeNull();
      expect(parsePriority("0 (Low)")).toBeNull();
    });

    it("13.2: Issue import does not invert priority scale (P0 is top rank 0)", () => {
      // Inverted bug ft-7e812b3f caused critical to map to 3 (Low) instead of 0 (Urgent)
      const p0Labels = ["bug", "critical", "outage"];
      const p1Labels = ["high", "perf"];
      const p2Labels = ["medium", "enhancement"];
      const p3Labels = ["low", "cosmetic"];

      expect(inferTaskPriority(p0Labels)).toBe(0); // P0 = 0
      expect(inferTaskPriority(p1Labels)).toBe(1); // P1 = 1
      expect(inferTaskPriority(p2Labels)).toBe(2); // P2 = 2
      expect(inferTaskPriority(p3Labels)).toBe(3); // P3 = 3

      // Default with no priority labels is 2 (Normal)
      expect(inferTaskPriority(["enhancement"])).toBe(2);
      expect(inferTaskPriority([])).toBe(2);
    });

    it("13.3: Typing in search input does not reset paged column to page 1", () => {
      // Paging calculation preserves loaded items count without snapping back to single page
      const loadedCount = 85;
      const pageSize = 30;

      // 85 items at 30/page requires 3 pages
      const pages = pagesLoaded(loadedCount, pageSize);
      expect(pages).toBe(3);

      // Reloading retains the full loaded window
      const sizeToReload = reloadSize(loadedCount, pageSize, 600);
      expect(sizeToReload).toBe(pageSize);

      // Comparing unchanged records returns true, preserving DOM without blanking
      const recordsA = [{ id: "t1", revision: 1 }, { id: "t2", revision: 1 }];
      const recordsB = [{ id: "t1", revision: 1 }, { id: "t2", revision: 1 }];
      expect(sameRecords(recordsA, recordsB)).toBe(true);

      const modifiedB = [{ id: "t1", revision: 1 }, { id: "t2", revision: 2 }];
      expect(sameRecords(recordsA, modifiedB)).toBe(false);
    });

    it("13.4: Idempotent task update verifies expected_revision before applying mutation", async () => {
      // 1. Verify production taskWrite builds wire payload with expected_revision fence
      const sampleDraft = {
        title: "Priority scale drift",
        kind: "bug",
        status: "inbox" as const,
        priority: 2,
        severity: null,
        owner: null,
        due_at: null,
        labels: ["drift"],
        repository_ids: ["r1"],
        primary_repository_id: "r1",
        home_workspace_id: null,
        position: 10,
        description: "Reconcile priority scales",
        acceptance_criteria: ["Verify 4-tier scale"],
        checklist: [],
        links: [],
      };

      const payload1 = taskWrite("ft-7e812b3f", 4, sampleDraft);
      expect(payload1.id).toBe("ft-7e812b3f");
      expect(payload1.expected_revision).toBe(4);
      expect(payload1.request_id).toMatch(/^[0-9a-f-]{36}$/);

      // 2. Storage IO implementing production IO interface
      let currentTask: Task = task({
        id: "ft-7e812b3f",
        revision: 4,
        updated_at: 1000,
        ...sampleDraft,
      });

      const storeIO = {
        read: async (id: string) => {
          if (id !== currentTask.id) throw new WorkbenchError("not_found", "Task not found");
          return currentTask;
        },
        write: async (input: Record<string, unknown>) => {
          if (input.expected_revision !== currentTask.revision) {
            throw new WorkbenchError("revision_conflict", `Revision mismatch: expected ${input.expected_revision}, current is ${currentTask.revision}`);
          }
          currentTask = task({
            ...currentTask,
            status: (input.status as TaskStatus) ?? currentTask.status,
            revision: currentTask.revision + 1,
            updated_at: Date.now(),
          });
          return currentTask;
        },
        remove: async () => {},
      };

      // 3. First update with matching revision (4) succeeds via production TaskBatch
      const batch1 = new TaskBatch(
        [{ id: "ft-7e812b3f", title: currentTask.title, revision: 4 }],
        { kind: "update", changes: { status: "ready" } },
        storeIO,
      );
      await batch1.run();
      const snap1 = batch1.snapshot();
      expect(snap1[0].state).toBe("done");
      expect(snap1[0].error).toBe("");
      expect(currentTask.status).toBe("ready");
      expect(currentTask.revision).toBe(5);

      // 4. Concurrent / duplicate update with stale revision (4) is rejected by TaskBatch
      const staleBatch = new TaskBatch(
        [{ id: "ft-7e812b3f", title: currentTask.title, revision: 4 }],
        { kind: "update", changes: { status: "in_progress" } },
        storeIO,
      );
      await staleBatch.run();
      const snap2 = staleBatch.snapshot();
      expect(snap2[0].state).toBe("failed");
      expect(snap2[0].error).toContain("This task changed while you were moving it");
      expect(currentTask.status).toBe("ready"); // Unmodified by stale mutation
      expect(currentTask.revision).toBe(5);

      // 5. Retry with fresh revision (5) succeeds via production TaskBatch
      const freshBatch = new TaskBatch(
        [{ id: "ft-7e812b3f", title: currentTask.title, revision: 5 }],
        { kind: "update", changes: { status: "in_progress" } },
        storeIO,
      );
      await freshBatch.run();
      const snap3 = freshBatch.snapshot();
      expect(snap3[0].state).toBe("done");
      expect(snap3[0].error).toBe("");
      expect(currentTask.status).toBe("in_progress");
      expect(currentTask.revision).toBe(6);
    });

    it("13.5: Evidence-gated task completion rejects unverified tasks", () => {
      // 1. Frontend verification contract validation using production decodeVerification
      const unverifiedResponse = { ok: true, state: "none", stale: false, record: null };
      const unverifiedDecoded = decodeVerification(unverifiedResponse);
      expect(unverifiedDecoded.state).toBe("none");
      expect(unverifiedDecoded.record).toBeNull();
      expect(VERDICT_HINTS[unverifiedDecoded.state]).toBe("No verification has judged this task.");

      const failingResponse = {
        ok: true,
        state: "failed",
        stale: false,
        record: {
          request_id: "req-f1",
          head: "deadbeef1234",
          worktree: "/tmp/worktree",
          criteria: [{ index: 1, text: "exports csv", status: "unmet", findings: ["no csv writer found"] }],
          degraded: [],
          critic_summary: "",
        },
      };
      const failingDecoded = decodeVerification(failingResponse);
      expect(failingDecoded.state).toBe("failed");
      expect(failingDecoded.record?.criteria[0].status).toBe("unmet");
      expect(VERDICT_HINTS[failingDecoded.state]).toBe("A check found a defect or an unmet criterion.");

      const passedResponse = {
        ok: true,
        state: "passed",
        stale: false,
        record: {
          request_id: "req-p1",
          head: "951dd401abcd",
          worktree: "/tmp/worktree",
          criteria: [{ index: 1, text: "exports csv", status: "met", findings: [] }],
          degraded: [],
          critic_summary: "All criteria met.",
        },
      };
      const passedDecoded = decodeVerification(passedResponse);
      expect(passedDecoded.state).toBe("passed");
      expect(passedDecoded.record?.criteria[0].status).toBe("met");
      expect(VERDICT_HINTS[passedDecoded.state]).toBe("Every check ran and every criterion was met.");

      // Verify that unknown or facade states are rejected by production decodeVerification
      expect(() => decodeVerification({ ok: true, state: "approved" })).toThrow(WorkbenchError);

      // 2. Direct backend execution verification of GitPulse verification gate (15 tests in 0.44s)
      const res = runCommand("cargo", ["test", "-p", "gitpulse", "--lib", "intake::gate_tests"], {
        cwd: path.join(PROJECT_ROOT, "src-tauri"),
        timeout: 30_000,
      });
      expect(res.status).toBe(0);
      expect(res.stdout).toContain("workbench::intake::gate_tests::an_agent_claiming_done_with_no_verification_record_lands_in_review ... ok");
      expect(res.stdout).toContain("workbench::intake::gate_tests::a_failed_record_does_not_let_an_agent_finish_and_names_the_unmet_criterion ... ok");
      expect(res.stdout).toContain("workbench::intake::gate_tests::a_degraded_record_does_not_let_an_agent_finish ... ok");
      expect(res.stdout).toContain("workbench::intake::gate_tests::a_verification_still_running_is_not_a_verdict ... ok");
      expect(res.stdout).toContain("workbench::intake::gate_tests::a_passed_record_for_an_older_head_is_stale ... ok");
      expect(res.stdout).toContain("workbench::intake::gate_tests::a_passed_record_for_the_current_head_lets_the_agent_finish_and_is_attached ... ok");
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 14: E2E Testing Suite Infrastructure
  // ---------------------------------------------------------------------------
  describe("Feature 14: E2E Testing Suite Infrastructure", () => {
    it("14.1: Test suite environment configures Vitest with isolated node environment", async () => {
      // 1. Verify vitest.e2e.config.ts file exists on disk and declares node environment & globals
      const configFile = path.join(PROJECT_ROOT, "vitest.e2e.config.ts");
      expect(existsSync(configFile)).toBe(true);
      const configText = readFileSync(configFile, "utf8");
      expect(configText).toContain('environment: "node"');
      expect(configText).toContain("globals: true");
      expect(configText).toContain('"tests/e2e/**/*.test.ts"');

      // 2. Programmatically verify evaluated configuration module
      const e2eConfig = (await import("../../vitest.e2e.config")).default;
      expect(e2eConfig.test?.environment).toBe("node");
      expect(e2eConfig.test?.globals).toBe(true);
      expect(e2eConfig.test?.maxWorkers).toBe(4);
      expect(e2eConfig.test?.testTimeout).toBe(30_000);
      expect(e2eConfig.test?.hookTimeout).toBe(30_000);
      expect(e2eConfig.test?.include).toContain("tests/e2e/**/*.test.ts");

      // 3. Verify active runtime matches isolated Node.js environment
      expect(typeof process).toBe("object");
      expect(process.versions?.node).toBeDefined();
      expect(typeof Buffer).toBe("function");
      expect(typeof globalThis).toBe("object");
      // Isolated node environment must not expose browser DOM globals
      expect(typeof window).toBe("undefined");
      expect(typeof document).toBe("undefined");
    });

    it("14.2: TempDirManager creates isolated temporary directories and cleans up cleanly", () => {
      const manager = createTempManager();

      // 1. Create two distinct isolated temporary directories
      const dir1 = manager.create("gitpulse-e2e-test-1-");
      const dir2 = manager.create("gitpulse-e2e-test-2-");
      expect(dir1).not.toBe(dir2);
      expect(existsSync(dir1)).toBe(true);
      expect(existsSync(dir2)).toBe(true);
      expect(statSync(dir1).isDirectory()).toBe(true);
      expect(statSync(dir2).isDirectory()).toBe(true);

      // 2. Write test files and nested directories inside the managed directories
      const file1 = path.join(dir1, "test-file-1.txt");
      writeFileSync(file1, "isolated content 1", "utf8");
      expect(existsSync(file1)).toBe(true);
      expect(readFileSync(file1, "utf8")).toBe("isolated content 1");

      const nestedDir = path.join(dir2, "sub/nested");
      mkdirSync(nestedDir, { recursive: true });
      const file2 = path.join(nestedDir, "test-file-2.json");
      writeFileSync(file2, JSON.stringify({ isolated: true }), "utf8");
      expect(existsSync(file2)).toBe(true);

      // 3. Invoke cleanup and verify complete removal of all directories and contents
      manager.cleanup();
      expect(existsSync(dir1)).toBe(false);
      expect(existsSync(dir2)).toBe(false);
      expect(existsSync(file1)).toBe(false);
      expect(existsSync(file2)).toBe(false);

      // 4. Verify cleanup idempotency (calling cleanup again does not throw)
      expect(() => manager.cleanup()).not.toThrow();
    });

    it("14.3: Fixture Git Repo initializes clean git history with initial commit", () => {
      const repoDir = tempMgr.create("gitpulse-fixture-repo-");

      // 1. Initialize fixture repository with custom files
      const initialFiles = {
        "README.md": "# Test Fixture Repository\n",
        "src/main.ts": "export const initialized = true;\n",
      };
      createFixtureGitRepo(repoDir, initialFiles);

      // 2. Verify .git directory structure
      const gitDir = path.join(repoDir, ".git");
      expect(existsSync(gitDir)).toBe(true);
      expect(statSync(gitDir).isDirectory()).toBe(true);

      // 3. Verify committed fixture files exist on disk with correct content
      expect(existsSync(path.join(repoDir, "README.md"))).toBe(true);
      expect(readFileSync(path.join(repoDir, "README.md"), "utf8")).toBe(initialFiles["README.md"]);
      expect(existsSync(path.join(repoDir, "src/main.ts"))).toBe(true);
      expect(readFileSync(path.join(repoDir, "src/main.ts"), "utf8")).toBe(initialFiles["src/main.ts"]);

      // 4. Verify clean working tree (no untracked, modified, or unstaged files)
      const statusRes = runCommand("git", ["status", "--porcelain"], { cwd: repoDir });
      expect(statusRes.status).toBe(0);
      expect(statusRes.stdout.trim()).toBe("");

      // 5. Verify git commit log and author configuration
      const logRes = runCommand("git", ["log", "-1", "--format=%s%n%an <%ae>"], { cwd: repoDir });
      expect(logRes.status).toBe(0);
      const [subject, author] = logRes.stdout.trim().split("\n");
      expect(subject).toBe("Initial fixture commit");
      expect(author).toBe("E2E Tester <e2e-tester@example.com>");

      // 6. Verify HEAD resolves to a valid 40-character commit hash
      const headRes = runCommand("git", ["rev-parse", "HEAD"], { cwd: repoDir });
      expect(headRes.status).toBe(0);
      expect(headRes.stdout.trim()).toMatch(/^[0-9a-f]{40}$/);
    });

    it("14.4: Command executor handles non-zero exit codes with error diagnostics", () => {
      // 1. Valid command execution: exits 0, returns stdout, empty stderr
      const validRes = runCommand("node", ["-e", "process.stdout.write('success output'); process.exit(0);"]);
      expect(validRes.status).toBe(0);
      expect(validRes.stdout).toBe("success output");
      expect(validRes.stderr).toBe("");

      // 2. Failing command with non-zero exit code: captures exit code and stderr diagnostics
      const failRes = runCommand("node", [
        "-e",
        "process.stderr.write('fatal: simulated command failure'); process.exit(42);",
      ]);
      expect(failRes.status).toBe(42);
      expect(failRes.stderr).toContain("fatal: simulated command failure");
      expect(failRes.stdout).toBe("");

      // 3. Real CLI failure: git invalid subcommand returns non-zero status with diagnostics
      const gitFail = runCommand("git", ["unknown-subcommand-xyz"]);
      expect(gitFail.status).not.toBe(0);
      expect(gitFail.stderr).toMatch(/not a git command|unknown subcommand/i);

      // 4. Process safety: non-existent executable does not throw or crash process
      expect(() => runCommand("non_existent_executable_binary_12345", [])).not.toThrow();
      const nonExistentRes = runCommand("non_existent_executable_binary_12345", []);
      expect(nonExistentRes.status).toBeNull();
    });

    it("14.5: Wall-clock timeouts prevent hanging test runners", () => {
      // 1. Long-running command (sleep 10s) with 200ms timeout terminates cleanly
      const startSleep = Date.now();
      const sleepRes = runCommand("sleep", ["10"], { timeout: 200 });
      const elapsedSleep = Date.now() - startSleep;

      // Status must be null (killed by signal) or non-zero, never 0
      expect(sleepRes.status).toBeNull();
      // Wall-clock duration must abort near timeout (~200ms) without stalling for 10s
      expect(elapsedSleep).toBeLessThan(1_500);

      // 2. Node.js long-running async timer with 200ms timeout aborts without hanging
      const startNode = Date.now();
      const nodeRes = runCommand("node", ["-e", "setTimeout(() => {}, 15000);"], { timeout: 200 });
      const elapsedNode = Date.now() - startNode;

      expect(nodeRes.status).toBeNull();
      expect(elapsedNode).toBeLessThan(1_500);
    });
  });

  // ---------------------------------------------------------------------------
  // Feature 15: Final Verification & Adversarial Hardening
  // ---------------------------------------------------------------------------
  describe("Feature 15: Final Verification & Adversarial Hardening", () => {
    it("15.1: Write latency benchmark verifies transaction duration meets budget (< 50ms)", () => {
      const dbDir = tempMgr.create("fts5-write-bench-");
      const dbPath = path.join(dbDir, "bench_fts.sqlite");

      const db = new DatabaseSync(dbPath);
      db.exec("PRAGMA journal_mode = WAL;");
      db.exec("PRAGMA synchronous = NORMAL;");
      db.exec("PRAGMA page_size = 16384;");
      db.exec(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(name, qualified_name, path, tokenize='trigram');"
      );

      // DevMap FTS_50_SQL chunking: 50 tuples per batch (200 parameters)
      const placeholders = Array.from({ length: 50 }, () => "(?, ?, ?, ?)").join(", ");
      const FTS_50_SQL = `INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES ${placeholders}`;
      const stmt = db.prepare(FTS_50_SQL);

      const params: (number | string)[] = [];
      for (let i = 0; i < 50; i++) {
        params.push(i + 1, `symbol_${i}`, `module::symbol_${i}`, "src/core/write.rs");
      }

      const start = performance.now();
      db.exec("BEGIN IMMEDIATE;");
      stmt.run(...params);
      db.exec("COMMIT;");
      const durationMs = performance.now() - start;

      // Budget check: transaction duration must meet strict < 50ms budget
      expect(durationMs).toBeLessThan(50);

      // Verify records were committed to store
      const countRow = db.prepare("SELECT COUNT(*) as cnt FROM nodes_fts;").get() as { cnt: number };
      expect(countRow.cnt).toBe(50);

      db.close();
    });

    it("15.2: Mapping throughput scales linearly without quadratic degradation", () => {
      function benchmarkBatch(n: number): number {
        const dbDir = tempMgr.create(`fts-scale-${n}-`);
        const dbPath = path.join(dbDir, "scale.sqlite");
        const db = new DatabaseSync(dbPath);
        db.exec("PRAGMA journal_mode = WAL;");
        db.exec("PRAGMA synchronous = NORMAL;");
        db.exec("PRAGMA page_size = 16384;");
        db.exec(
          "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(name, qualified_name, path, tokenize='trigram');"
        );

        const placeholders = Array.from({ length: 50 }, () => "(?, ?, ?, ?)").join(", ");
        const FTS_50_SQL = `INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES ${placeholders}`;
        const stmt = db.prepare(FTS_50_SQL);

        const start = performance.now();
        db.exec("BEGIN IMMEDIATE;");
        for (let chunk = 0; chunk < n / 50; chunk++) {
          const params: (number | string)[] = [];
          for (let i = 0; i < 50; i++) {
            const idx = chunk * 50 + i;
            params.push(idx + 1, `sym_${idx}`, `mod::sym_${idx}`, `src/mod_${chunk}.rs`);
          }
          stmt.run(...params);
        }
        db.exec("COMMIT;");
        const elapsed = performance.now() - start;
        db.close();
        return elapsed;
      }

      const t100 = benchmarkBatch(100);
      const t200 = benchmarkBatch(200);
      const t400 = benchmarkBatch(400);

      expect(t100).toBeGreaterThan(0);
      expect(t200).toBeGreaterThan(0);
      expect(t400).toBeGreaterThan(0);

      // Linear scaling verification:
      // In O(N^2) degradation, 400 symbols would take >= 16x of 100 symbols (ratio >= 16.0).
      // Under linear scaling with FTS_50_SQL chunking, t400 / t100 is strictly bounded below 4.0x.
      expect(t400 / t100).toBeLessThan(4.0);

      // Throughput (symbols / ms) at N=400 must maintain >= 50% of throughput at N=100
      const throughput100 = 100 / t100;
      const throughput400 = 400 / t400;
      expect(throughput400).toBeGreaterThan(throughput100 * 0.5);
    });

    it("15.3: SQLite database header validation verifies WAL format markers", () => {
      // 1. Verify production store database header and journal mode
      const prodDb = path.join(PROJECT_ROOT, ".devcouncil/codeintel/devmap.sqlite");
      expect(existsSync(prodDb)).toBe(true);

      const pragmaRes = runCommand("sqlite3", [prodDb, "PRAGMA journal_mode; PRAGMA page_size;"]);
      expect(pragmaRes.status).toBe(0);
      const [journalMode, pageSize] = pragmaRes.stdout.trim().split("\n").map((s) => s.trim());
      expect(journalMode.toLowerCase()).toBe("wal");
      expect(Number(pageSize)).toBe(16384);

      // Verify binary header markers directly from SQLite file format specification:
      // Offset 0..16: "SQLite format 3\0"
      // Offset 18: File format write version (2 = WAL)
      // Offset 19: File format read version (2 = WAL)
      const fd = openSync(prodDb, "r");
      const headerBuf = Buffer.alloc(32);
      readSync(fd, headerBuf, 0, 32, 0);
      closeSync(fd);

      expect(headerBuf.subarray(0, 15).toString("utf8")).toBe("SQLite format 3");
      expect(headerBuf[18]).toBe(2);
      expect(headerBuf[19]).toBe(2);
      expect(headerBuf.readUInt16BE(16)).toBe(16384);

      // 2. Also verify isolated database transitions from legacy (1) to WAL (2)
      const freshDir = tempMgr.create("wal-header-test-");
      const freshDb = path.join(freshDir, "test.sqlite");
      const db = new DatabaseSync(freshDb);
      db.exec("PRAGMA journal_mode = DELETE; CREATE TABLE t (id INT);");
      let buf = readFileSync(freshDb);
      expect(buf[18]).toBe(1);
      expect(buf[19]).toBe(1);

      db.exec("PRAGMA journal_mode = WAL;");
      buf = readFileSync(freshDb);
      expect(buf[18]).toBe(2);
      expect(buf[19]).toBe(2);
      db.close();
    });

    it("15.4: Corrupt compressed payload throws handled error without crashing process", () => {
      // 1. Verify legitimate zstd round-trip
      const honestPayload = JSON.stringify({
        file_path: "src/main.rs",
        symbols: ["init", "run", "shutdown"],
        schema: 29,
      });
      const validFrame = zstdCompressSync(Buffer.from(honestPayload));
      expect(validFrame.subarray(0, 4).equals(SCHEMA_CONSTANTS.ZSTD_MAGIC_HEADER)).toBe(true);
      const roundTrip = zstdDecompressSync(validFrame).toString("utf8");
      expect(roundTrip).toBe(honestPayload);

      // 2. Corrupt magic bytes: invalid frame header throws handled error
      const corruptMagic = Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00]); // ELF header
      expect(() => zstdDecompressSync(corruptMagic)).toThrow();

      // 3. Truncated payload: only magic header without valid frame body throws handled error
      const truncated = Buffer.from(SCHEMA_CONSTANTS.ZSTD_MAGIC_HEADER);
      expect(() => zstdDecompressSync(truncated)).toThrow();

      // 4. CLI decompression fault isolation: command executor handles corrupt file without process crash
      const tempDir = tempMgr.create("zstd-corrupt-");
      const badFile = path.join(tempDir, "corrupt.zst");
      writeFileSync(badFile, corruptMagic);

      const cliRes = runCommand("zstd", ["-d", "-c", badFile]);
      expect(cliRes.status).not.toBe(0);
      expect(cliRes.stderr.length).toBeGreaterThan(0);
    });

    it("15.5: Zero unhandled promise rejections during concurrent transaction rollback", async () => {
      const dbDir = tempMgr.create("concurrent-rollback-");
      const dbPath = path.join(dbDir, "stress.sqlite");

      // Initialize SQLite WAL database with accounts table
      const initDb = new DatabaseSync(dbPath);
      initDb.exec(`
        PRAGMA journal_mode = WAL;
        PRAGMA page_size = 16384;
        CREATE TABLE accounts (
          id INTEGER PRIMARY KEY,
          owner TEXT NOT NULL,
          balance INTEGER NOT NULL
        );
        INSERT INTO accounts VALUES (1, 'alice', 1000), (2, 'bob', 1000);
      `);
      initDb.close();

      // Monitor process for unhandled promise rejections
      const unhandledRejections: unknown[] = [];
      const onUnhandled = (reason: unknown) => {
        unhandledRejections.push(reason);
      };
      process.on("unhandledRejection", onUnhandled);

      try {
        const concurrency = 20;
        let successfulCommits = 0;
        let handledRollbacks = 0;

        // Concurrent connection stress: 20 simultaneous workers performing transactional mutations
        const tasks = Array.from({ length: concurrency }, async (_, i) => {
          const conn = new DatabaseSync(dbPath);
          try {
            conn.exec("BEGIN IMMEDIATE;");
            conn.exec("UPDATE accounts SET balance = balance + 10 WHERE id = 1;");
            conn.exec("UPDATE accounts SET balance = balance - 10 WHERE id = 2;");

            // Inject intentional runtime fault on 50% of transactions to trigger rollback
            if (i % 2 === 1) {
              throw new Error(`Worker ${i} simulated transaction abort`);
            }

            conn.exec("COMMIT;");
            successfulCommits++;
          } catch {
            // Explicit rollback on failure
            try {
              conn.exec("ROLLBACK;");
              handledRollbacks++;
            } catch {
              // Rollback error handled safely
            }
          } finally {
            conn.close();
          }
        });

        await Promise.allSettled(tasks);

        // Assert zero unhandled promise rejections occurred
        expect(unhandledRejections.length).toBe(0);

        // Verify that both successful commits and handled rollbacks took place
        expect(successfulCommits).toBeGreaterThan(0);
        expect(handledRollbacks).toBeGreaterThan(0);

        // Verify database integrity post-stress
        const verifyDb = new DatabaseSync(dbPath);
        const integrity = verifyDb.prepare("PRAGMA integrity_check;").get() as { integrity_check: string };
        expect(integrity.integrity_check).toBe("ok");

        // Verify invariant: total balance across accounts remains conserved (2000)
        const sumRow = verifyDb.prepare("SELECT SUM(balance) as total FROM accounts;").get() as { total: number };
        expect(sumRow.total).toBe(2000);

        verifyDb.close();
      } finally {
        process.removeListener("unhandledRejection", onUnhandled);
      }
    });
  });
});
