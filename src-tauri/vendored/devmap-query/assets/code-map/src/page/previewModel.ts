/**
 * The repo-map page's reading of `map_preview::build_preview_payload`.
 *
 * Pure: no DOM. Everything the Navigator and the sidebar show is read here,
 * through the `DATA` and `META` roots, so the Rust contract test
 * (`every_key_the_page_reads_is_one_the_payload_carries`) can scan this file
 * and fail when it names a key the payload does not carry. Wire data is
 * untrusted: every field is narrowed, none is cast.
 */

import { asRecord } from "../record";

export interface LanguageStyle {
  color: string;
  label: string;
  official: boolean;
}

export interface PreviewSubsystem {
  id: string;
  area: string;
  summary: string;
  entryPoints: string[];
  criticalFiles: string[];
  neighbors: string[];
  handoffPaths: string[];
  /** Capped samples per role: the payload carries no per-role totals. */
  roleFiles: Record<string, string[]>;
  languages: Array<[string, number]>;
  fileCount: number;
  documentation: boolean;
}

export interface CapCount {
  shown: number;
  available: number;
  /** The producer's real total, or null when it is not known. */
  total: number | null;
}

export interface PreviewModel {
  subsystems: PreviewSubsystem[];
  languageTotals: Array<[string, number]>;
  palette: Record<string, LanguageStyle>;
  neutralColor: string;
  linguistVersion: string;
  indexedTotal: number;
  inSubsystems: number;
  generatedHead: string;
  graphHtml: string;
  handoffPathsComputed: boolean;
  roleFilesComputed: boolean;
  unresolved: Array<{ from: string; text: string }>;
  unresolvedTotal: number;
  unresolvedBySubsystem: Record<string, { total: number; listed: number }>;
  liveness: {
    entryRoots: CapCount;
    unwiredCandidates: CapCount;
    unreachableFiles: CapCount;
    deadSymbolCandidates: CapCount;
    unreachableUnreliable: boolean;
  };
  edgeCount: number;
  nodeCount: number;
}

const NEUTRAL_FALLBACK = "#6e7681";

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}

function pairs(value: unknown): Array<[string, number]> {
  const out: Array<[string, number]> = [];
  for (const row of list(value)) {
    if (Array.isArray(row) && typeof row[0] === "string") out.push([row[0], count(row[1])]);
  }
  return out;
}

function capCount(live: Record<string, unknown>, counts: Record<string, unknown>, key: string): CapCount {
  const meta = asRecord(counts[key]);
  const shown = list(live[key]).length;
  const total = meta && typeof meta.total === "number" && Number.isSafeInteger(meta.total) && meta.total >= shown ? meta.total : null;
  const available = meta && typeof meta.available === "number" && Number.isSafeInteger(meta.available) ? Math.max(meta.available, shown) : shown;
  return { shown, available, total };
}

export function readPreview(DATA: Record<string, unknown>): PreviewModel {
  const META = asRecord(DATA.meta) ?? {};
  const palette: Record<string, LanguageStyle> = {};
  for (const [id, raw] of Object.entries(asRecord(META.palette) ?? {})) {
    const style = asRecord(raw);
    if (style) palette[id] = { color: text(style.color) || NEUTRAL_FALLBACK, label: text(style.label) || id, official: style.official === true };
  }
  const subsystems: PreviewSubsystem[] = [];
  for (const raw of list(DATA.subsystems)) {
    const row = asRecord(raw);
    const area = row ? text(row.area) : "";
    if (!row || !area) continue;
    const roleFiles: Record<string, string[]> = {};
    for (const [role, paths] of Object.entries(asRecord(row.role_files) ?? {})) roleFiles[role] = strings(paths);
    subsystems.push({
      id: text(row.id) || area,
      area,
      summary: text(row.summary),
      entryPoints: strings(row.entry_points),
      criticalFiles: strings(row.critical_files),
      neighbors: strings(row.neighbors),
      handoffPaths: strings(row.handoff_paths),
      roleFiles,
      languages: pairs(row.languages),
      fileCount: count(row.file_count),
      documentation: row.documentation === true,
    });
  }
  const coverage = asRecord(META.coverage) ?? {};
  const LIVE = asRecord(DATA.liveness) ?? {};
  const counts = asRecord(LIVE.counts) ?? {};
  const unresolved: Array<{ from: string; text: string }> = [];
  for (const raw of list(DATA.unresolved_handoffs)) {
    const row = asRecord(raw);
    if (row) unresolved.push({ from: text(row.from), text: text(row.text) });
  }
  const bySubsystem: Record<string, { total: number; listed: number }> = {};
  for (const [area, raw] of Object.entries(asRecord(DATA.unresolved_handoffs_by_subsystem) ?? {})) {
    const row = asRecord(raw);
    if (row) bySubsystem[area] = { total: count(row.total), listed: count(row.listed) };
  }
  return {
    subsystems,
    languageTotals: pairs(META.language_totals),
    palette,
    neutralColor: text(META.neutral_color) || NEUTRAL_FALLBACK,
    linguistVersion: text(META.linguist_version),
    indexedTotal: count(coverage.indexed_total),
    inSubsystems: count(coverage.in_subsystems),
    generatedHead: text(META.generated_head),
    graphHtml: text(META.graph_html),
    handoffPathsComputed: META.handoff_paths_computed === true,
    roleFilesComputed: META.role_files_computed === true,
    unresolved,
    unresolvedTotal: count(DATA.unresolved_handoffs_total),
    unresolvedBySubsystem: bySubsystem,
    liveness: {
      entryRoots: capCount(LIVE, counts, "entry_roots"),
      unwiredCandidates: capCount(LIVE, counts, "unwired_candidates"),
      unreachableFiles: capCount(LIVE, counts, "unreachable_files"),
      deadSymbolCandidates: capCount(LIVE, counts, "dead_symbol_candidates"),
      unreachableUnreliable: LIVE.liveness_unreachable_unreliable === true,
    },
    nodeCount: list(DATA.nodes).length,
    edgeCount: list(DATA.links).length,
  };
}

/** "N" when the total is known and shown, "N of M" when capped, with an honest lower bound otherwise. */
export function capText(cap: CapCount): string {
  const n = (v: number) => v.toLocaleString("en-US");
  if (cap.total !== null) return cap.shown < cap.total ? `${n(cap.shown)} of ${n(cap.total)}` : n(cap.total);
  if (cap.available > cap.shown) return `${n(cap.shown)} of at least ${n(cap.available)}`;
  return `${n(cap.shown)} shown, total unknown`;
}

/** The headline's unplaced-handoff clause; empty when every handoff found an edge. */
export function unplacedText(total: number): string {
  if (total <= 0) return "";
  return `${total.toLocaleString("en-US")} handoff${total === 1 ? "" : "s"} unplaced`;
}

/**
 * The test sample for a subsystem, stated honestly. The payload caps every
 * role list and carries no per-role total, so the page can name the sample's
 * size and nothing more: "N of M" would need an M it does not have.
 */
export function testsSampleText(shown: number): string {
  if (shown === 0) return "No test files in the sample; the payload carries no total for this area.";
  return `Sample of ${shown.toLocaleString("en-US")}; the payload carries no total for this area.`;
}
