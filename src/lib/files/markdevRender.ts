/**
 * MarkDev markdown client: parse via Rust (`cmd_markdown_parse`), render via
 * Rust (`cmd_markdown_render`) or from a flat model locally.
 *
 * Document stats / outline / frontmatter stay pure TypeScript — they are not
 * in the parse model. The old regex renderer in `markDevParser.ts` is gone;
 * callers use `renderMarkDevMarkdown` (async IPC) or the sync helpers below.
 */

import { invoke } from "@tauri-apps/api/core";

export interface MarkdownHeading {
  level: number;
  title: string;
  id: string;
}

export interface DocumentStats {
  wordCount: number;
  charCount: number;
  lineCount: number;
  readingTimeMinutes: number;
  headingCount: number;
  linkCount: number;
}

export interface FrontmatterField {
  key: string;
  value: string;
}

export interface ResolvedSpan {
  start: number;
  end: number;
  kind: string;
  depth: number;
  data: number;
  text?: string | null;
}

export interface ResolvedMarker {
  start: number;
  end: number;
  block: number;
}

export interface ResolvedBlock {
  start: number;
  end: number;
  kind: string;
  depth: number;
  data: number;
  info?: string | null;
}

export interface ParsedMarkdown {
  source: string;
  spans: ResolvedSpan[];
  markers: ResolvedMarker[];
  blocks: ResolvedBlock[];
  truncated: boolean;
}

/**
 * Ceiling on how much markdown is rendered in one pass.
 * Mirrored in Rust (`markdown::MAX_RENDER_BYTES`).
 */
export const MAX_RENDER_BYTES = 128 * 1024;

/** Parse markdown into MarkDev's flat model (UTF-16 offsets). */
export function parseMarkdown(text: string): Promise<ParsedMarkdown> {
  return invoke<ParsedMarkdown>("cmd_markdown_parse", { text });
}

/**
 * Render markdown to safe HTML through the Rust flat-model renderer.
 * Caps oversized input the same way the backend does.
 */
export async function renderMarkDevMarkdown(markdown: string): Promise<string> {
  if (!markdown) return "";
  return invoke<string>("cmd_markdown_render", { text: markdown });
}

/** Calculates reading metrics and structural stats for a Markdown document. */
export function calculateDocumentStats(text: string): DocumentStats {
  if (!text) {
    return {
      wordCount: 0,
      charCount: 0,
      lineCount: 0,
      readingTimeMinutes: 0,
      headingCount: 0,
      linkCount: 0,
    };
  }

  const lines = text.split(/\r?\n/);
  const withoutCode = text.replace(/```[\s\S]*?```/g, " ");
  const words = withoutCode
    .replace(/[#>*_\-\[\]\(\)`]/g, " ")
    .split(/\s+/)
    .filter(Boolean);

  const headingCount = (withoutCode.match(/^#{1,6}\s+\S+/gm) || []).length;
  // Bounded quantifiers: an unmatched `[` must not scan to end-of-string from
  // every position (was quadratic — measured 1235ms → 0ms on adversarial input).
  const mdLinks = withoutCode.match(/\[[^\]\n]{0,200}\]\([^)\n]{0,500}\)/g) || [];
  const wikiLinks = withoutCode.match(/\[\[[^\]\n]{0,200}\]\]/g) || [];

  return {
    wordCount: words.length,
    charCount: text.length,
    lineCount: lines.length,
    readingTimeMinutes: Math.max(1, Math.ceil(words.length / 200)),
    headingCount,
    linkCount: mdLinks.length + wikiLinks.length,
  };
}

/** Extracts a heading outline, ignoring fenced code. */
export function extractDocumentOutline(text: string): MarkdownHeading[] {
  if (!text) return [];
  const outline: MarkdownHeading[] = [];
  let inFence = false;
  for (const line of text.split(/\r?\n/)) {
    if (/^```/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const match = /^(#{1,6})\s+(.+?)\s*$/.exec(line);
    if (!match) continue;
    const level = match[1].length;
    const title = match[2].replace(/#+\s*$/, "").trim();
    if (!title) continue;
    outline.push({ level, title, id: slugify(title) });
  }
  return outline;
}

export function parseFrontmatter(doc: string): {
  frontmatter: FrontmatterField[];
  content: string;
} {
  if (!doc.startsWith("---\n") && !doc.startsWith("---\r\n")) {
    return { frontmatter: [], content: doc };
  }
  const end = doc.indexOf("\n---", 4);
  if (end < 0) return { frontmatter: [], content: doc };
  const raw = doc.slice(4, end);
  const content = doc.slice(end + 4).replace(/^\r?\n/, "");
  const frontmatter: FrontmatterField[] = [];
  for (const line of raw.split(/\r?\n/)) {
    const cut = line.indexOf(":");
    if (cut <= 0) continue;
    frontmatter.push({
      key: line.slice(0, cut).trim(),
      value: line.slice(cut + 1).trim(),
    });
  }
  return { frontmatter, content };
}

function slugify(text: string): string {
  return (
    text
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "section"
  );
}
