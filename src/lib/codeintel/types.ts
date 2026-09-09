/**
 * In-process code intelligence types matching `src-tauri/src/codeintel/mod.rs`.
 */

export interface CodeintelSymbolHit {
  symbol_name: string;
  file_path: string;
  kind: string;
  span_start_line: number;
  span_end_line: number;
  source_span: string;
  /**
   * Set when `source_span` is empty because the file could not be read — a map
   * generation newer than the checkout, or a file since deleted — rather than
   * because the symbol has no body. Without it the two render identically.
   */
  source_unavailable_reason?: string | null;
  /** Source bytes withheld by the query budget; not an unreadable-file error. */
  source_span_omitted_bytes?: number;
  score: number;
}

export interface CodeintelEdge {
  source_file: string;
  target_file: string;
  source_symbol: string;
  target_symbol: string;
  confidence: number;
}

export interface CodeintelDeadSymbol {
  symbol_name: string;
  file_path: string;
  confidence: number;
  is_exempt: boolean;
  exemption_reason?: string | null;
}

export interface CodeintelRungHistogram {
  deterministic: number;
  high: number;
  speculative: number;
  filtered_out: number;
}

export interface CodeintelResponse<T> {
  /** Null (or absent on older hosts): this query did not verify the current tree. */
  source_freshness?: boolean | null;
  available: boolean;
  reason?: string | null;
  items: T[];
  total: number;
  shown: number;
  truncated: boolean;
  /** Producer stopped early — distinct from budget truncation. Omitted when absent. */
  walk_incomplete?: string;
  /** Population across the resolution ladder before any min_rung filter. Omitted when absent. */
  rungs?: CodeintelRungHistogram;
}

export interface CodeintelStatus {
  available: boolean;
  /** A stale generation can remain available for navigation. */
  is_fresh?: boolean | null;
  freshness_reason?: string | null;
  pending_count?: number | null;
  db_path: string;
  generation_id?: number | null;
  total_files?: number | null;
  total_symbols?: number | null;
  total_edges?: number | null;
  reason?: string | null;
}

export type CodeintelRung = "deterministic" | "high" | "speculative";

export interface CodeintelNeighbors {
  target: string;
  callers: CodeintelResponse<CodeintelEdge>;
  callees: CodeintelResponse<CodeintelEdge>;
}

export interface CodeintelBlastLayer {
  depth: number;
  nodes: string[];
  node_count: number;
  nodes_omitted: number;
  lowest_confidence?: number | null;
}

export interface CodeintelBlastRadius {
  seeds: string[];
  unmatched_targets: string[];
  layers: CodeintelResponse<CodeintelBlastLayer>;
  total_impacted: number;
}

export interface CodeintelLayeredImpact {
  available: boolean;
  reason?: string | null;
  edges: CodeintelResponse<CodeintelEdge>;
  blast_radius: CodeintelBlastRadius;
}

export interface CodeintelAffectedTest {
  path: string;
  depth: number;
  symbols: string[];
  reached_symbols: number;
}

export interface CodeintelAffectedTests {
  available: boolean;
  reason?: string | null;
  targets: string[];
  tests: CodeintelResponse<CodeintelAffectedTest>;
  blast_radius: CodeintelBlastRadius;
  fail_closed: boolean;
  fail_closed_reason?: string | null;
}

export interface CodeintelExploreDefinition {
  symbol_name: string;
  file_path: string;
  kind: string;
  id: string;
}

export interface CodeintelExplore {
  available: boolean;
  reason?: string | null;
  definitions: CodeintelResponse<CodeintelExploreDefinition>;
  blast_radius: CodeintelBlastRadius;
  limit: number;
}

export interface CodeintelCloneGroup {
  size: number;
  members: string[];
}

export interface CodeintelClones {
  available: boolean;
  reason?: string | null;
  groups: CodeintelResponse<CodeintelCloneGroup>;
  signed_symbols: number;
  unsigned_symbols: number;
}

export type DevmapLookup = "explicit_env" | "saved_config" | "path_search" | "test_override";

export interface DevmapBuildOutcome {
  ok: boolean;
  binary: string;
  lookup: DevmapLookup;
  exit_code?: number | null;
  stdout: string;
  stderr: string;
  timed_out: boolean;
  report?: unknown;
}

export interface DevmapCliStatus {
  available: boolean;
  binary?: string | null;
  lookup?: DevmapLookup | null;
  reason?: string | null;
  status?: DevmapStatusPayload | null;
}

/** Fields the Map freshness strip reads from `devmap status --json`. */
export interface DevmapStatusPayload {
  is_fresh?: boolean;
  schema_outdated?: boolean;
  schema_version?: number;
  expected_schema_version?: number;
  generation_id?: number;
  node_count?: number;
  edge_count?: number;
  pending_count?: number;
  degraded_reason?: string | null;
  coverage_gaps?: Record<string, unknown> | null;
  [key: string]: unknown;
}

export interface RepoMapCapMeta {
  shown: number;
  total: number;
  truncated: boolean;
}

export interface RepoMapUnwiredMeta extends RepoMapCapMeta {
  excluded_coverage_loss: number;
  excluded_import_blind: number;
}

export interface RepoMapDeadSymbolMeta extends RepoMapCapMeta {
  count: number;
  by_confidence?: {
    shown?: { extracted?: number; inferred?: number; ambiguous?: number };
    total?: { extracted?: number; inferred?: number; ambiguous?: number };
  };
}

export interface RepoMapSubsystemsMeta extends RepoMapCapMeta {
  dropped_no_area: number;
  neighbors_shown: number;
  neighbors_total: number;
  neighbors_truncated: boolean;
  handoff_paths_shown: number;
  handoff_paths_total: number;
  handoff_paths_truncated: boolean;
  role_files_shown: number;
  role_files_total: number;
  role_files_truncated: boolean;
  neighbors_endpoints_unresolved: number;
}

export interface RepoMapLivenessMeta {
  engine?: string | null;
  dead_symbol: RepoMapDeadSymbolMeta;
  entry_roots: RepoMapCapMeta;
  subsystems: RepoMapSubsystemsMeta;
  important_files: RepoMapCapMeta;
  unwired: RepoMapUnwiredMeta;
  unavailable: Record<string, string>;
}

export interface RepoMapSubsystem {
  area: string;
  summary: string;
  entry_points: string[];
  critical_files: string[];
  neighbors: string[];
  handoff_paths: string[];
  /** Capped samples per role. Always pair with `role_file_counts`. */
  role_files: Record<string, string[]>;
  /** Real per-role totals beside the capped samples. */
  role_file_counts: Record<string, number>;
}

export interface RepoMapDocument {
  map_engine?: string | null;
  generated_head: string;
  indexed_hash: string;
  content_fingerprint: string;
  graph_degraded: boolean;
  graph_degraded_reason: string;
  /** When true, ignore `unreachable_files` entirely. */
  liveness_unreachable_unreliable: boolean;
  entry_roots: string[];
  subsystems: RepoMapSubsystem[];
  unwired_candidates: string[];
  dead_symbol_candidates: string[];
  unreachable_files: string[];
  liveness_meta: RepoMapLivenessMeta;
  languages: string[];
  important_files: string[];
  package_managers: string[];
  test_commands: string[];
  resolution_rate?: unknown;
  meta?: unknown;
  dead_clusters?: unknown;
  dead_clusters_truncated?: boolean;
  dead_clusters_incomplete?: string | null;
}

export interface RepoMapLoad {
  available: boolean;
  reason?: string | null;
  path?: string | null;
  map?: RepoMapDocument | null;
}

export interface DevmapPreviewFileResult {
  file_path: string;
  available: boolean;
  reason?: string | null;
  report?: DevmapPreviewReport | null;
}

export interface DevmapPreviewCaller {
  source_file?: string;
  source_symbol?: string;
  target_symbol?: string;
  confidence?: number;
  [key: string]: unknown;
}

export interface DevmapPreviewReport {
  file_path: string;
  parse_status: string;
  delta_available: boolean;
  file_is_indexed: boolean;
  compared_against: string;
  degraded_reason?: string | null;
  symbols: unknown[];
  bodies_not_compared: number;
  ambiguous_callers: number;
  broken_callers: CodeintelResponse<DevmapPreviewCaller>;
}

export interface DevmapPreviewOutcome {
  available: boolean;
  binary?: string | null;
  lookup?: DevmapLookup | null;
  reason?: string | null;
  files: DevmapPreviewFileResult[];
  cancelled: boolean;
}

/** Decision from the live-index gate (`decide_live_refresh`). */
export type LiveRefreshDecision =
  | "refresh"
  | "skip_fresh"
  | "skip_building"
  | "skip_schema_outdated"
  | "skip_unavailable";

export interface LiveRefreshFacts {
  available: boolean;
  is_fresh: boolean;
  schema_ok: boolean;
  already_building: boolean;
}

export interface LiveRefreshOutcome {
  decision: LiveRefreshDecision;
  facts: LiveRefreshFacts;
  build?: DevmapBuildOutcome | null;
  reason?: string | null;
}

/** Envelope from `cmd_devmap_viz` / `cmd_devmap_map_preview` / docs graph. */
export type GraphVizKind = "code_graph" | "map_preview";
/** Client-built doc navigator graph — not a Rust `GraphVizKind` variant. */
export type DocGraphKind = "doc_graph";
export type CanvasGraphKind = GraphVizKind | DocGraphKind;

export interface GraphVizNodeFlag {
  flag: string;
  confidence?: string;
}

export interface GraphVizNode {
  id: string;
  name: string;
  kind?: string;
  path?: string;
  area?: string;
  community?: string;
  language?: string;
  lang?: string;
  line?: number;
  degree?: number;
  flags?: GraphVizNodeFlag[];
  /** Map-preview sizing hint. */
  val?: number;
  file_count?: number;
  summary?: string;
  entry?: boolean;
  /** Optional layout coordinates when the payload supplies them. */
  x?: number;
  y?: number;
}

export interface GraphVizLink {
  source: string;
  target: string;
  kind?: string;
  confidence?: number | null;
  /** Number of distinct source relationships aggregated into this link. */
  evidence_count?: number;
  resolution?: string;
  label?: string;
}

export interface GraphVizCounts {
  nodes_shown: number;
  nodes_total: number;
  nodes_truncated: boolean;
  links_shown?: number;
  links_total?: number;
  max_nodes?: number;
}

export interface GraphVizPayload {
  level?: string;
  nodes: GraphVizNode[];
  links: GraphVizLink[];
  counts?: GraphVizCounts;
  communities?: Record<string, number>;
  generation_id?: number | string | null;
  subsystems?: unknown[];
  unresolved_handoffs?: unknown[];
  liveness?: unknown;
  meta?: {
    coverage?: { indexed_total?: number; in_subsystems?: number };
    [key: string]: unknown;
  };
}

export interface GraphVizLoad {
  available: boolean;
  reason?: string | null;
  path?: string | null;
  /** Rust sends GraphVizKind; the doc navigator may set DocGraphKind locally. */
  kind: CanvasGraphKind;
  payload?: GraphVizPayload | null;
}

/* ── Multi-repo workspace registry (devmap workspace.json) ─────────────── */

export interface WorkspaceRepoEntry {
  name: string;
  root: string;
  db: string;
  db_path: string;
}

export interface WorkspaceSnapshot {
  version: number;
  registry_root: string;
  registry_path: string;
  repos: WorkspaceRepoEntry[];
}

export interface WorkspaceRegisterResult {
  name: string;
  root: string;
  replaced: boolean;
  registry_path: string;
}

export interface WorkspaceUnregisterResult {
  name: string;
  removed: boolean;
  registry_path: string;
}

export interface WorkspaceRepoUnavailable {
  repo: string;
  reason: string;
}

export interface WorkspaceFederatedHit {
  repo: string;
  symbol_name: string;
  file_path: string;
  kind: string;
  span_start_line: number;
  span_end_line: number;
  source_span: string;
  source_unavailable_reason?: string | null;
  score: number;
}

export interface WorkspaceSearchResult {
  items: WorkspaceFederatedHit[];
  repos_queried: number;
  unavailable: WorkspaceRepoUnavailable[];
  total: number;
  shown: number;
  hidden: number;
  truncated: boolean;
  /** True when TF-IDF name ranking was requested — not AI search. */
  semantic: boolean;
}

export interface WorkspaceLinkCandidate {
  from_repo: string;
  from_file: string;
  module_specifier: string;
  to_repo: string;
  evidence: string;
}

export interface WorkspaceLinksResult {
  links: WorkspaceLinkCandidate[];
  count: number;
  repos_considered: number;
}
