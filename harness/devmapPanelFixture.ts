import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  DevmapCliStatus,
  RepoMapCapMeta,
  RepoMapDocument,
  RepoMapLoad,
  RepoMapSubsystem,
  WorkspaceLinksResult,
} from "../src/lib/codeintel/types";
import type { BrokenLink, DocsSearchHit } from "../src/lib/docs/client";

/**
 * What the Code → Map panel is shown by the devmap harness.
 *
 * Every shape is the complete wire type, not a smaller one: a fixture short of
 * a field makes `invoke` answer `undefined`, the panel reads `.length` off it
 * and throws mid-render, and the torn-down effects make the run look clean.
 */

export const FIXTURE_REPO = "/fixture/map";

const cap = (shown: number, total: number, truncated = false): RepoMapCapMeta => ({ shown, total, truncated });
const files = (prefix: string, count: number): string[] =>
  Array.from({ length: count }, (_, i) => `${prefix}/file-${i + 1}.ts`);

export function subsystem(area: string, over: Partial<RepoMapSubsystem> = {}): RepoMapSubsystem {
  return {
    area,
    summary: `${area}: harness area`,
    entry_points: [],
    critical_files: [],
    neighbors: [],
    handoff_paths: [],
    role_files: {},
    role_file_counts: {},
    ...over,
  };
}

/** A path with no break opportunity and far wider than any pane. */
export const LONG_PATH = `src/${"deeply-nested-directory/".repeat(8)}component.ts`;

/** The subsystem list the checks switch between. `src/big` has more than
 * five of everything; `src/small` has exactly five critical files and fewer
 * than five of the rest, so it must show no expander anywhere. */
export function fixtureSubsystems(): RepoMapSubsystem[] {
  return [
    subsystem("src/big", {
      entry_points: files("src/big/entry", 8),
      critical_files: files("src/big/critical", 6),
      neighbors: ["src/small", "docs", "tools", "scripts", "lib/a", "lib/b", "vendor/outside"],
      role_files: { tests: files("src/big/tests", 12) },
      role_file_counts: { tests: 20 },
    }),
    subsystem("src/small", {
      entry_points: files("src/small/entry", 3),
      critical_files: files("src/small/critical", 5),
      neighbors: ["src/big", "docs"],
    }),
    subsystem("docs"),
    subsystem("tools"),
    subsystem("scripts"),
    subsystem("lib/a"),
    subsystem("lib/b"),
    subsystem("lib/c"),
  ];
}

export function mapDocument(over: Partial<RepoMapDocument> = {}): RepoMapDocument {
  const subsystems = over.subsystems ?? fixtureSubsystems();
  return {
    generated_head: "h",
    indexed_hash: "h",
    content_fingerprint: "h",
    graph_degraded: false,
    graph_degraded_reason: "",
    liveness_unreachable_unreliable: false,
    entry_roots: files("src/roots", 3),
    subsystems,
    unwired_candidates: [...files("src/unwired", 9)],
    dead_symbol_candidates: Array.from({ length: 12 }, (_, i) => `src/dead.ts::unused_${i + 1}`),
    unreachable_files: files("src/unreachable", 6),
    liveness_meta: {
      dead_symbol: { ...cap(12, 12), count: 12 },
      entry_roots: cap(3, 3),
      subsystems: {
        ...cap(subsystems.length, subsystems.length), dropped_no_area: 0,
        neighbors_shown: 9, neighbors_total: 9, neighbors_truncated: false,
        handoff_paths_shown: 0, handoff_paths_total: 0, handoff_paths_truncated: false,
        role_files_shown: 12, role_files_total: 20, role_files_truncated: true,
        neighbors_endpoints_unresolved: 0,
      },
      important_files: cap(7, 7),
      unwired: { ...cap(9, 30, true), excluded_coverage_loss: 0, excluded_import_blind: 0 },
      unavailable: {},
    },
    languages: ["TypeScript", "Rust", "Svelte"],
    important_files: [LONG_PATH, ...files("src/important", 6)],
    package_managers: ["bun"],
    test_commands: ["bun run test"],
    ...over,
  };
}

export const mapLoad = (map: RepoMapDocument): RepoMapLoad => ({
  available: true, reason: null, path: `${FIXTURE_REPO}/repo_map.json`, failure: null, map,
});

/** `degraded` is a current index that nonetheless reports a reason it is not whole. */
export type StatusMode = "fresh" | "stale" | "degraded" | "failed" | "pending";

export interface PanelScenario {
  map: RepoMapLoad;
  status: StatusMode;
}

const statusLoad = (fresh: boolean, degraded = !fresh): DevmapCliStatus => ({
  available: true,
  reason: null,
  status: {
    is_fresh: fresh,
    schema_outdated: false,
    schema_version: 99,
    expected_schema_version: 99,
    generation_id: 12,
    node_count: 2400,
    edge_count: 9100,
    pending_count: fresh ? 0 : 3,
    degraded_reason: degraded ? "stored extraction payload is obsolete; rebuild with the current analyzer" : null,
    coverage_gaps: null,
  },
});

const searchHits = (query: string): DocsSearchHit[] =>
  Array.from({ length: 8 }, (_, i) => ({
    path: `docs/guide-${i + 1}.md`, title: `Guide ${i + 1}`, context: `${query} hit ${i + 1}`, line: i + 1, score: 1 - i / 10,
  }));

const brokenLinks = (): BrokenLink[] =>
  Array.from({ length: 9 }, (_, i) => ({ source: `docs/source-${i + 1}.md`, target: `missing-${i + 1}.md` }));

const linkCandidates = (): WorkspaceLinksResult => ({
  links: Array.from({ length: 7 }, (_, i) => ({
    from_repo: "app", from_file: `src/use-${i + 1}.ts`, module_specifier: `shared/mod-${i + 1}`, to_repo: "shared", evidence: `import ${i + 1}`,
  })),
  coverage_gaps: [], count: 7, repos_considered: 2,
});

/** The state the installed IPC handler answers from. The checks swap it. */
const initialScenario: PanelScenario = { map: mapLoad(mapDocument()), status: "fresh" };

export const panel = {
  scenario: initialScenario,
  /** Commands invoked that no case below answers. */
  unexpected: new Set<string>(),
  releaseStatus: null as (() => void) | null,
  /** How many times the map was read, so a check can wait for a background refresh. */
  mapReads: 0,
};

/**
 * Install the IPC the panel talks to. The set of commands is what opening a
 * repository tab and the map's own views ask for, the same list
 * `harness/diagnostics.html` answers; anything else is recorded in
 * `panel.unexpected` and refused.
 */
export function installPanelIpc(): void {
  mockIPC((cmd, args) => {
    const arg = (name: string): string => {
      const value: unknown = args && typeof args === "object" ? Reflect.get(args, name) : undefined;
      return typeof value === "string" ? value : "";
    };
    switch (cmd) {
      case "cmd_resolve_repo": return { path: arg("path") || arg("repoPath") || FIXTURE_REPO, name: "map", is_bare: false };
      case "cmd_devmap_repo_map":
        panel.mapReads += 1;
        // A copy, as the IPC bridge hands back: a refresh gives the panel new objects.
        return structuredClone(panel.scenario.map);
      case "cmd_devmap_status": {
        switch (panel.scenario.status) {
          case "fresh": return statusLoad(true);
          case "stale": return statusLoad(false);
          case "degraded": return statusLoad(true, true);
          case "failed": throw new Error("Fixture status probe failed");
          case "pending": return new Promise<DevmapCliStatus>((resolve) => { panel.releaseStatus = () => resolve(statusLoad(true)); });
        }
        return null;
      }
      case "cmd_docs_refresh": return { noteCount: 9, truncated: false, skippedOversized: 0, skippedUnreadable: 0 };
      case "cmd_docs_broken_links": return brokenLinks();
      case "cmd_docs_search": return searchHits(arg("query"));
      case "cmd_workspace_link_candidates": return linkCandidates();
      case "cmd_workspace_sync": return { repos: [] };
      case "cmd_devmap_viz": case "cmd_devmap_map_preview":
        return { available: false, reason: "Harness: no graph", path: null, kind: "code_graph", payload: null };
      case "cmd_external_tools_status":
        return { devmap: { tool: "devmap", installed: true, path: "/fixture/devmap", lookup: "path", version: "1", reason: null, source_checkout: null, install_ready: true, install_block: null, install_command: "" } };
      case "cmd_devcouncil_init":
        return { repo: FIXTURE_REPO, state_dir: `${FIXTURE_REPO}/.devmap`, exclude: { status: "already_ignored", source: "harness" }, workspace_registry: null, workspace_reason: null, skipped_untrusted: [], skipped_unavailable: [], devmap_available: false };
      case "cmd_devmap_maybe_refresh":
        return { decision: "skip_unavailable", facts: { available: false, is_fresh: false, schema_ok: false, already_building: false }, reason: null };
      case "cmd_list_branches": case "cmd_get_status": case "cmd_degraded_subsystems": case "cmd_diagnostic_log_tail": return [];
      case "cmd_list_tags": return { tags: [], truncated: false };
      case "cmd_stash_list": return { entries: [], truncated: false };
      case "cmd_branch_stats": return { updates: [], capped: false, compute_failures: 0, compared_to: "main" };
      case "cmd_get_commit_graph": return { nodes: [], edges: [], total_commits: 0, truncated: false };
      case "cmd_daemon_status": return { running: false, pid: null, version: null, app_version: "0.0.0", mismatch: false, error: null };
      case "cmd_list_repo_files": return [];
      case "cmd_repo_operation": case "cmd_watch_repo": case "cmd_unwatch_repo": case "cmd_last_fetch_at":
      case "cmd_note_tab_activated": case "cmd_set_recent_menu": return null;
      default:
        if (cmd.startsWith("plugin:")) return null;
        panel.unexpected.add(cmd);
        throw new Error(`Unconfigured harness command: ${cmd}`);
    }
  }, { shouldMockEvents: true });
}
