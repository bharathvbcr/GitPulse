/**
 * Doc vault client — search, broken links, backlinks, graph, rename.
 * Production callers for `check:ipc`.
 */

import { invoke } from "@tauri-apps/api/core";

export interface DocsStatus {
  noteCount: number;
  truncated: boolean;
  skippedOversized: number;
  skippedUnreadable: number;
}

export interface DocsSearchHit {
  path: string;
  title: string;
  context: string;
  line: number;
  score: number;
}

export interface BrokenLink {
  source: string;
  target: string;
}

export interface DocBacklink {
  path: string;
  title: string;
  context: string;
  line: number;
  offset: number;
}

export interface DocGraphNode {
  path: string;
  title: string;
  tags: string[];
  degree: number;
  depth: number;
  x: number;
  y: number;
}

export interface DocGraphEdge {
  source: number;
  target: number;
}

export interface DocGraph {
  nodes: DocGraphNode[];
  edges: DocGraphEdge[];
  total_notes: number;
  truncated: boolean;
}

export interface DocRenameOutcome {
  from: string;
  to: string;
  rewrittenPaths: string[];
  linksRewritten: number;
}

export function docsRefresh(repoPath: string): Promise<DocsStatus> {
  return invoke<DocsStatus>("cmd_docs_refresh", { repoPath });
}

export function docsStatus(repoPath: string): Promise<DocsStatus> {
  return invoke<DocsStatus>("cmd_docs_status", { repoPath });
}

export function docsSearch(
  repoPath: string,
  query: string,
  limit?: number,
): Promise<DocsSearchHit[]> {
  return invoke<DocsSearchHit[]>("cmd_docs_search", { repoPath, query, limit });
}

export function docsBrokenLinks(repoPath: string): Promise<BrokenLink[]> {
  return invoke<BrokenLink[]>("cmd_docs_broken_links", { repoPath });
}

export function docsBacklinks(repoPath: string, path: string): Promise<DocBacklink[]> {
  return invoke<DocBacklink[]>("cmd_docs_backlinks", { repoPath, path });
}

export function docsGraph(
  repoPath: string,
  opts?: { focus?: string; depth?: number; tag?: string; folder?: string },
): Promise<DocGraph> {
  return invoke<DocGraph>("cmd_docs_graph", {
    repoPath,
    focus: opts?.focus ?? null,
    depth: opts?.depth ?? null,
    tag: opts?.tag ?? null,
    folder: opts?.folder ?? null,
  });
}

export function docsRename(
  repoPath: string,
  from: string,
  to: string,
): Promise<{ policy: unknown; output: DocRenameOutcome }> {
  return invoke("cmd_docs_rename", { repoPath, from, to });
}
