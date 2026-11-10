/**
 * The wire shapes the shared code-map modules read.
 *
 * `devmap-query` owns the payloads (`viz::build_payload`,
 * `map_preview::build_preview_payload`); these interfaces are the reader's
 * view of them, kept to the fields the layout, navigation and renderer touch.
 * No DOM, no framework: GitPulse type-checks this file with its own tsc.
 */

/** Doc-graph layout extent: supplied `x`/`y` live in a 1000 x 1000 box. */
export const DOC_GRAPH_EXTENT = 1000;

/**
 * The slice of a repo-map subsystem the area tree reads. Both the preview
 * payload's subsystems and GitPulse's `RepoMapSubsystem` satisfy it.
 */
export interface AreaSource {
  area: string;
  summary: string;
}

export interface GraphVizNodeFlag {
  flag: string;
  confidence?: string;
}

export interface GraphVizNode {
  id: string;
  name: string;
  /** DevMap classification; for subsystems, every attributed file is documentation. */
  documentation?: boolean;
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
