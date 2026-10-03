/**
 * MarkDev markdown client: render via Rust (`cmd_markdown_render`), which
 * runs MarkDev's own HTML renderer.
 *
 * The outline and the frontmatter come back with the render — one pass wrote
 * the heading ids and listed them, so an outline entry always names an id the
 * page has. Only the reading stats stay TypeScript; they are not rendering.
 */

import { invoke } from "../ipc/invoke";

export interface MarkdownHeading {
  level: number;
  /** Plain text of the heading, markup removed. */
  title: string;
  /** The rendered element's id, before {@link prepareRenderedMarkdown} prefixes it. */
  id: string;
}

/** A rendered note (`markdown::RenderedMarkdown`). */
export interface RenderedMarkdown {
  /** Sanitized body HTML; styled by `.gp-markdown`. */
  html: string;
  headings: MarkdownHeading[];
  /** The frontmatter block's `key: value` lines; not part of `html`. */
  frontmatter: FrontmatterField[];
  /** UTF-8 bytes past {@link MAX_RENDER_BYTES} that were not rendered. */
  omittedBytes: number;
}

/** Where a rendered note lives, so its relative pictures can be read. */
export interface MarkdownLocation {
  /** Absolute repository root. */
  repoPath: string;
  /** Repository-relative path of the note. */
  filePath: string;
}

export const EMPTY_RENDER: RenderedMarkdown = Object.freeze({
  html: "",
  headings: [],
  frontmatter: [],
  omittedBytes: 0,
}) as RenderedMarkdown;

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

/**
 * Ceiling on how much markdown is rendered in one pass.
 * Mirrored in Rust (`markdown::MAX_RENDER_BYTES`).
 */
export const MAX_RENDER_BYTES = 128 * 1024;

/**
 * Renders markdown to sanitized HTML with MarkDev's renderer.
 *
 * With a `location` the note's relative pictures are read from inside the
 * repository and embedded; without one (a commit message) nothing is read.
 */
export async function renderMarkDevMarkdown(
  markdown: string,
  location: MarkdownLocation | null = null,
): Promise<RenderedMarkdown> {
  if (!markdown) return EMPTY_RENDER;
  return invoke<RenderedMarkdown>("cmd_markdown_render", {
    text: markdown,
    repoPath: location?.repoPath ?? null,
    filePath: location?.filePath ?? null,
  });
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
