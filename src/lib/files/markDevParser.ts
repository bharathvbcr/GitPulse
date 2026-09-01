import {
  tokenizeLine,
  tokenClass,
  detectLanguageFromPath,
  type SupportedLanguage,
} from "./syntaxHighlight";

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

/**
 * Calculates reading metrics and structural stats for a Markdown document.
 */
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

  const lines = text.split("\n");
  const lineCount = lines.length;
  const charCount = text.length;

  // Strip code blocks and frontmatter for word count
  const cleanText = text
    .replace(/^---[\s\S]*?---/, "")
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`]+`/g, "");

  const words = cleanText.trim().match(/\b[\w'-]+\b/g);
  const wordCount = words ? words.length : 0;
  const readingTimeMinutes = Math.max(1, Math.ceil(wordCount / 200));

  const headings = text.match(/^#{1,6}\s+.+$/gm);
  const headingCount = headings ? headings.length : 0;

  const links = text.match(/\[([^\]]+)\]\(([^)]+)\)|\[\[([^\]]+)\]\]/g);
  const linkCount = links ? links.length : 0;

  return {
    wordCount,
    charCount,
    lineCount,
    readingTimeMinutes,
    headingCount,
    linkCount,
  };
}

/**
 * Extracts table-of-contents outline from Markdown headings.
 */
export function extractDocumentOutline(text: string): MarkdownHeading[] {
  if (!text) return [];
  const headings: MarkdownHeading[] = [];
  const lines = text.split("\n");
  let inCodeBlock = false;

  for (const line of lines) {
    if (line.trim().startsWith("```")) {
      inCodeBlock = !inCodeBlock;
      continue;
    }
    if (inCodeBlock) continue;

    const match = line.match(/^(#{1,6})\s+(.+)$/);
    if (match) {
      const level = match[1].length;
      const rawTitle = match[2].trim().replace(/\*|_|`|==|~~/g, "");
      const id = slugify(rawTitle);
      headings.push({ level, title: rawTitle, id });
    }
  }

  return headings;
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .slice(0, 60);
}

function escapeHtml(str: string): string {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Parses YAML/TOML frontmatter block from start of Markdown document.
 */
export function parseFrontmatter(text: string): {
  frontmatter: FrontmatterField[];
  content: string;
} {
  const match = text.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?/);
  if (!match) {
    return { frontmatter: [], content: text };
  }

  const raw = match[1];
  const fields: FrontmatterField[] = [];
  const lines = raw.split("\n");

  for (const line of lines) {
    const colonIdx = line.indexOf(":");
    if (colonIdx !== -1) {
      const key = line.slice(0, colonIdx).trim();
      const val = line.slice(colonIdx + 1).trim().replace(/^["']|["']$/g, "");
      if (key) {
        fields.push({ key, value: val });
      }
    }
  }

  return {
    frontmatter: fields,
    content: text.slice(match[0].length),
  };
}

/**
 * Maps callout types to styling and icons.
 */
interface CalloutConfig {
  icon: string;
  label: string;
  borderClass: string;
  bgClass: string;
  textClass: string;
  badgeClass: string;
}

const CALLOUT_MAP: Record<string, CalloutConfig> = {
  NOTE: {
    icon: `<svg class="w-4 h-4 text-sky-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"></circle><line x1="12" y1="16" x2="12" y2="12"></line><line x1="12" y1="8" x2="12.01" y2="8"></line></svg>`,
    label: "Note",
    borderClass: "border-sky-500/50",
    bgClass: "bg-sky-500/10",
    textClass: "text-sky-300",
    badgeClass: "bg-sky-500/20 text-sky-300 border-sky-500/40",
  },
  TIP: {
    icon: `<svg class="w-4 h-4 text-emerald-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83"></path></svg>`,
    label: "Tip",
    borderClass: "border-emerald-500/50",
    bgClass: "bg-emerald-500/10",
    textClass: "text-emerald-300",
    badgeClass: "bg-emerald-500/20 text-emerald-300 border-emerald-500/40",
  },
  IMPORTANT: {
    icon: `<svg class="w-4 h-4 text-purple-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"></circle><polygon points="12 8 8 12 12 16 16 12 12 8"></polygon></svg>`,
    label: "Important",
    borderClass: "border-purple-500/50",
    bgClass: "bg-purple-500/10",
    textClass: "text-purple-300",
    badgeClass: "bg-purple-500/20 text-purple-300 border-purple-500/40",
  },
  WARNING: {
    icon: `<svg class="w-4 h-4 text-amber-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z"></path><line x1="12" y1="9" x2="12" y2="13"></line><line x1="12" y1="17" x2="12.01" y2="17"></line></svg>`,
    label: "Warning",
    borderClass: "border-amber-500/50",
    bgClass: "bg-amber-500/10",
    textClass: "text-amber-300",
    badgeClass: "bg-amber-500/20 text-amber-300 border-amber-500/40",
  },
  CAUTION: {
    icon: `<svg class="w-4 h-4 text-rose-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2"></polygon><line x1="12" y1="8" x2="12" y2="12"></line><line x1="12" y1="16" x2="12.01" y2="16"></line></svg>`,
    label: "Caution",
    borderClass: "border-rose-500/50",
    bgClass: "bg-rose-500/10",
    textClass: "text-rose-300",
    badgeClass: "bg-rose-500/20 text-rose-300 border-rose-500/40",
  },
};

/**
 * High-performance, robust, and safe MarkDev Markdown to HTML renderer.
 */
/**
 * Keep a URL only if its scheme is safe to put in an `href` or `src`.
 *
 * Markdown rendered here is repository content, which is untrusted: a README
 * can carry `[click](javascript:...)`, and this used to interpolate the target
 * straight into the attribute. Quotes are already escaped by the time links are
 * rendered, so an attribute breakout was not possible, but the scheme itself
 * was never checked.
 *
 * The app's CSP (`script-src 'self'`, no `unsafe-inline`) should stop such a
 * URL from executing. That is a second line, not the first, and it is enforced
 * by three different webviews across the supported platforms.
 *
 * Relative and anchor targets are kept — they are the common case in a
 * repository — and anything carrying an unrecognised scheme is dropped.
 */
function safeUrl(raw: string): string | null {
  const url = raw.trim();
  if (url === "") return null;
  // A scheme is [a-z][a-z0-9+.-]* before the first ':', and only these are
  // allowed. Control characters are stripped first: "java\tscript:" and
  // "java\nscript:" are treated as the scheme by some parsers.
  const bare = url.replace(/[\u0000-\u001f\u007f]/g, "");
  const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(bare);
  if (!scheme) return url; // relative path, anchor, or protocol-relative
  return /^(https?|mailto|tel)$/i.test(scheme[1]) ? url : null;
}

export function renderMarkDevMarkdown(markdown: string): string {
  if (!markdown) return "";

  // 1. Extract frontmatter
  const { frontmatter, content } = parseFrontmatter(markdown);
  let frontmatterHtml = "";

  if (frontmatter.length > 0) {
    const rows = frontmatter
      .map(
        (f) =>
          `<span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-surface border border-border/70 text-[11px] font-mono"><strong class="text-accent font-semibold">${escapeHtml(
            f.key
          )}:</strong> <span class="text-textPrimary">${escapeHtml(
            f.value
          )}</span></span>`
      )
      .join(" ");

    frontmatterHtml = `
      <div class="mb-6 p-3 rounded-xl border border-border/70 bg-surface/50 shadow-sm select-none">
        <div class="text-[10px] font-mono uppercase tracking-wider text-textMuted mb-2 flex items-center gap-1.5 font-bold">
          <span class="w-1.5 h-1.5 rounded-full bg-accent"></span>
          <span>Metadata / Frontmatter</span>
        </div>
        <div class="flex flex-wrap gap-2">
          ${rows}
        </div>
      </div>
    `;
  }

  // 2. Pre-process blocks
  const codeBlocks: string[] = [];
  const mathBlocks: string[] = [];
  const tableBlocks: string[] = [];

  let processed = content;

  // Fenced code blocks
  processed = processed.replace(
    /```([a-zA-Z0-9_\-#+]*)\r?\n([\s\S]*?)```/g,
    (_match, lang, code) => {
      const idx = codeBlocks.length;
      const normalizedLang = (lang || "plaintext").toLowerCase().trim();
      const codeId = `code-block-${idx}`;

      // Check if mermaid
      if (normalizedLang === "mermaid") {
        codeBlocks.push(`
          <div class="my-4 rounded-xl border border-border/80 bg-surface overflow-hidden shadow-card">
            <div class="flex items-center justify-between px-3 py-1.5 border-b border-border/60 bg-surfaceHover/50 select-none">
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full bg-purple-400"></span>
                <span class="font-mono text-[10px] font-bold text-purple-400 uppercase">MERMAID DIAGRAM</span>
              </div>
              <button
                type="button"
                class="gp-btn !py-0.5 !px-2 text-[10px] copy-code-btn"
                data-code="${escapeHtml(code)}"
                title="Copy diagram source"
              >
                Copy
              </button>
            </div>
            <div class="p-4 bg-background/50 font-mono text-xs text-textPrimary overflow-x-auto">
              <pre class="leading-relaxed"><code>${escapeHtml(code)}</code></pre>
            </div>
          </div>
        `);
      } else {
        // Highlight code lines
        const langType = detectLanguageFromPath(`file.${normalizedLang}`) as SupportedLanguage;
        const codeLines = code.split("\n");
        const highlightedLines = codeLines
          .map((line: string) => {
            const tokens = tokenizeLine(line, langType);
            if (tokens.length === 0) return "&nbsp;";
            return tokens
              .map(
                (t) =>
                  `<span class="${tokenClass(t.type)}">${escapeHtml(
                    t.text
                  )}</span>`
              )
              .join("");
          })
          .join("\n");

        codeBlocks.push(`
          <div class="my-4 rounded-xl border border-border/80 bg-surface overflow-hidden shadow-card group">
            <div class="flex items-center justify-between px-3 py-1.5 border-b border-border/60 bg-surfaceHover/40 select-none">
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full bg-accent"></span>
                <span class="font-mono text-[10px] font-bold text-accent uppercase">${escapeHtml(
                  normalizedLang.toUpperCase()
                )}</span>
              </div>
              <button
                type="button"
                class="gp-btn !py-0.5 !px-2 text-[10px] copy-code-btn"
                data-code="${escapeHtml(code)}"
                title="Copy code"
              >
                Copy
              </button>
            </div>
            <div class="p-3.5 bg-background font-mono text-xs overflow-x-auto leading-relaxed gp-scroll">
              <pre><code id="${codeId}">${highlightedLines}</code></pre>
            </div>
          </div>
        `);
      }

      return `<!--CODE_BLOCK_${idx}-->`;
    }
  );

  // Display math $$ ... $$
  processed = processed.replace(
    /\$\$([\s\S]*?)\$\$/g,
    (_match, mathContent) => {
      const idx = mathBlocks.length;
      mathBlocks.push(`
        <div class="my-4 p-4 rounded-xl border border-border/80 bg-surface/60 text-center font-mono text-xs text-textPrimary shadow-sm overflow-x-auto">
          <div class="text-[9px] font-mono text-textMuted/60 uppercase tracking-widest mb-1 select-none">FORMULA</div>
          <div class="text-sm font-semibold tracking-wide text-cyan-300">${escapeHtml(
            mathContent.trim()
          )}</div>
        </div>
      `);
      return `<!--MATH_BLOCK_${idx}-->`;
    }
  );

  // Markdown Pipe Tables
  processed = processed.replace(
    /(?:^|\n)(\|[^\n]+\|\r?\n\|[-: |]+\|\r?\n(?:\|[^\n]+\|\r?\n?)+)/g,
    (_match, tableRaw) => {
      const idx = tableBlocks.length;
      const rows = tableRaw.trim().split("\n");
      if (rows.length < 2) return tableRaw;

      const headerCells = rows[0]
        .split("|")
        .slice(1, -1)
        .map((c: string) => c.trim());
      const alignCells = rows[1]
        .split("|")
        .slice(1, -1)
        .map((c: string) => {
          const s = c.trim();
          if (s.startsWith(":") && s.endsWith(":")) return "center";
          if (s.endsWith(":")) return "right";
          return "left";
        });

      const bodyRows = rows.slice(2);

      let theadHtml = "<tr>";
      headerCells.forEach((h: string, i: number) => {
        const align = alignCells[i] || "left";
        theadHtml += `<th class="px-3.5 py-2 text-${align} font-bold text-textPrimary text-xs border-b border-border/80 bg-surface/80">${renderInline(
          h
        )}</th>`;
      });
      theadHtml += "</tr>";

      let tbodyHtml = "";
      bodyRows.forEach((r: string) => {
        const cells = r.split("|").slice(1, -1);
        if (cells.length === 0) return;
        tbodyHtml += `<tr class="border-b border-border/40 hover:bg-surface/50 transition-colors">`;
        cells.forEach((c: string, i: number) => {
          const align = alignCells[i] || "left";
          tbodyHtml += `<td class="px-3.5 py-2 text-${align} text-textPrimary/90 text-xs">${renderInline(
            c.trim()
          )}</td>`;
        });
        tbodyHtml += "</tr>";
      });

      tableBlocks.push(`
        <div class="my-4 rounded-xl border border-border/70 bg-surface/40 overflow-hidden shadow-sm">
          <div class="overflow-x-auto gp-scroll">
            <table class="w-full text-left border-collapse">
              <thead>${theadHtml}</thead>
              <tbody>${tbodyHtml}</tbody>
            </table>
          </div>
        </div>
      `);

      return `\n<!--TABLE_BLOCK_${idx}-->\n`;
    }
  );

  // 3. Process line-by-line block structures (Headings, Callouts, Blockquotes, Checklists, Lists, Dividers)
  const lines = processed.split("\n");
  const outputLines: string[] = [];
  let inList = false;
  let inBlockquote = false;
  let currentBlockquoteLines: string[] = [];

  function flushBlockquote() {
    if (!inBlockquote || currentBlockquoteLines.length === 0) {
      inBlockquote = false;
      currentBlockquoteLines = [];
      return;
    }

    const firstLine = currentBlockquoteLines[0].trim();
    const calloutMatch = firstLine.match(/^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]/i);

    if (calloutMatch) {
      const type = calloutMatch[1].toUpperCase();
      const config = CALLOUT_MAP[type] || CALLOUT_MAP.NOTE;
      const bodyLines = currentBlockquoteLines.slice(1).join("<br />");

      outputLines.push(`
        <div class="my-4 p-3.5 rounded-xl border ${config.borderClass} ${config.bgClass} shadow-sm">
          <div class="flex items-center gap-2 font-bold text-xs ${config.textClass} mb-1.5 select-none">
            ${config.icon}
            <span>${config.label}</span>
          </div>
          <div class="text-xs text-textPrimary/95 leading-relaxed pl-6">
            ${bodyLines ? renderInline(bodyLines) : ""}
          </div>
        </div>
      `);
    } else {
      const quoteHtml = currentBlockquoteLines
        .map((l) => renderInline(l))
        .join("<br />");
      outputLines.push(`
        <blockquote class="border-l-2 border-accent/60 pl-3.5 py-1.5 my-3 text-textMuted italic bg-accent/5 rounded-r-xl text-xs leading-relaxed">
          ${quoteHtml}
        </blockquote>
      `);
    }

    inBlockquote = false;
    currentBlockquoteLines = [];
  }

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();

    // Check for block placeholders
    if (trimmed.startsWith("<!--CODE_BLOCK_") || trimmed.startsWith("<!--MATH_BLOCK_") || trimmed.startsWith("<!--TABLE_BLOCK_")) {
      if (inBlockquote) flushBlockquote();
      if (inList) {
        outputLines.push("</ul>");
        inList = false;
      }
      outputLines.push(trimmed);
      continue;
    }

    // Blockquotes & Callouts
    if (line.startsWith(">")) {
      if (inList) {
        outputLines.push("</ul>");
        inList = false;
      }
      inBlockquote = true;
      currentBlockquoteLines.push(line.replace(/^>\s?/, ""));
      continue;
    } else if (inBlockquote) {
      flushBlockquote();
    }

    // Horizontal Rule
    if (/^(?:---|\*\*\*|___)$/.test(trimmed)) {
      if (inList) {
        outputLines.push("</ul>");
        inList = false;
      }
      outputLines.push('<hr class="border-border/60 my-5" />');
      continue;
    }

    // Headings
    const headingMatch = line.match(/^(#{1,6})\s+(.+)$/);
    if (headingMatch) {
      if (inList) {
        outputLines.push("</ul>");
        inList = false;
      }
      const level = headingMatch[1].length;
      const rawTitle = headingMatch[2].trim();
      const slug = slugify(rawTitle);
      const titleHtml = renderInline(rawTitle);

      switch (level) {
        case 1:
          outputLines.push(
            `<h1 id="${slug}" class="text-xl font-bold text-textPrimary mt-7 mb-3 pb-2 border-b border-border/80 flex items-center gap-2 group"><span class="text-accent">#</span> <span>${titleHtml}</span></h1>`
          );
          break;
        case 2:
          outputLines.push(
            `<h2 id="${slug}" class="text-lg font-bold text-textPrimary mt-6 mb-2.5 pb-1.5 border-b border-border/60 flex items-center gap-2 group"><span class="text-accent/80">##</span> <span>${titleHtml}</span></h2>`
          );
          break;
        case 3:
          outputLines.push(
            `<h3 id="${slug}" class="text-base font-bold text-textPrimary mt-5 mb-2 flex items-center gap-1.5"><span class="text-accent/60">###</span> <span>${titleHtml}</span></h3>`
          );
          break;
        case 4:
          outputLines.push(
            `<h4 id="${slug}" class="text-sm font-bold text-textPrimary mt-4 mb-1.5">${titleHtml}</h4>`
          );
          break;
        case 5:
        case 6:
          outputLines.push(
            `<h${level} id="${slug}" class="text-xs font-bold text-textMuted uppercase tracking-wider mt-3 mb-1">${titleHtml}</h${level}>`
          );
          break;
      }
      continue;
    }

    // Checklists / Task lists
    const taskMatch = line.match(/^\s*-\s+\[([ xX])\]\s+(.*)$/);
    if (taskMatch) {
      if (inList) {
        outputLines.push("</ul>");
        inList = false;
      }
      const isChecked = taskMatch[1].toLowerCase() === "x";
      const taskText = renderInline(taskMatch[2]);
      outputLines.push(`
        <div class="flex items-center gap-2.5 my-1.5 text-xs select-none">
          <span class="w-4 h-4 rounded flex items-center justify-center text-[10px] font-bold ${
            isChecked
              ? "bg-accent text-white shadow-sm"
              : "border border-border/90 bg-surface text-transparent"
          }">
            ${isChecked ? "✓" : ""}
          </span>
          <span class="${isChecked ? "line-through text-textMuted/70" : "text-textPrimary"}">${taskText}</span>
        </div>
      `);
      continue;
    }

    // Bullet Lists
    const listMatch = line.match(/^\s*[-*+]\s+(.*)$/);
    if (listMatch) {
      if (!inList) {
        outputLines.push('<ul class="my-2 space-y-1 pl-4 list-disc text-textPrimary/90">');
        inList = true;
      }
      outputLines.push(`<li class="text-xs leading-relaxed">${renderInline(listMatch[1])}</li>`);
      continue;
    } else if (inList) {
      outputLines.push("</ul>");
      inList = false;
    }

    // Empty lines
    if (!trimmed) {
      continue;
    }

    // Paragraph
    outputLines.push(`<p class="my-2.5 leading-relaxed text-xs text-textPrimary/90">${renderInline(line)}</p>`);
  }

  if (inBlockquote) flushBlockquote();
  if (inList) outputLines.push("</ul>");

  let finalHtml = outputLines.join("\n");

  // Restore code blocks
  codeBlocks.forEach((block, i) => {
    finalHtml = finalHtml.replace(`<!--CODE_BLOCK_${i}-->`, block);
  });

  // Restore math blocks
  mathBlocks.forEach((block, i) => {
    finalHtml = finalHtml.replace(`<!--MATH_BLOCK_${i}-->`, block);
  });

  // Restore table blocks
  tableBlocks.forEach((block, i) => {
    finalHtml = finalHtml.replace(`<!--TABLE_BLOCK_${i}-->`, block);
  });

  return `${frontmatterHtml}<div class="markdev-prose">${finalHtml}</div>`;
}

/**
 * Renders inline Markdown constructs: bold, italic, strikethrough, highlights, inline math,
 * wikilinks, inline code, links, images.
 */
function renderInline(text: string): string {
  if (!text) return "";

  let out = escapeHtml(text);

  // Inline Code: `code`
  out = out.replace(
    /`([^`]+)`/g,
    '<code class="px-1.5 py-0.5 rounded bg-surface border border-border/70 text-amber-300 font-mono text-[11px] select-all">$1</code>'
  );

  // Inline Math: $math$
  out = out.replace(
    /\$([^\$\n]+)\$/g,
    '<span class="inline-flex items-center px-1.5 py-0.2 rounded bg-cyan-500/15 text-cyan-300 border border-cyan-500/30 font-mono text-[11px]">$1</span>'
  );

  // Highlights: ==highlight==
  out = out.replace(
    /==([^=]+)==/g,
    '<mark class="bg-amber-400/25 text-amber-200 px-1 py-0.5 rounded border border-amber-500/30 font-medium">$1</mark>'
  );

  // Strikethrough: ~~text~~
  out = out.replace(/~~([^~]+)~~/g, '<del class="line-through text-textMuted/70">$1</del>');

  // Wikilinks: [[Target|Alias]] or [[Target]]
  out = out.replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_m, target, alias) => {
    const label = alias || target;
    return `<span class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-300 font-mono text-[11px] border border-purple-500/30 font-medium"><span>[[</span><span class="text-textPrimary font-normal">${escapeHtml(
      label
    )}</span><span>]]</span></span>`;
  });

  // Images: ![alt](src)
  out = out.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, (_m, alt, src) => {
    const safe = safeUrl(src);
    // An image with an unusable source renders as its alt text rather than a
    // broken frame, so the reader still sees what the author wrote.
    if (safe === null) {
      return `<span class="text-textMuted font-mono text-[11px]">${alt || "image"}</span>`;
    }
    return `<span class="block my-3 rounded-xl border border-border/80 overflow-hidden bg-surface max-w-xl"><img src="${safe}" alt="${alt}" class="w-full h-auto object-contain max-h-[450px]" loading="lazy" />${
      alt ? `<span class="block text-center py-1 text-[10px] text-textMuted font-mono border-t border-border/60 bg-surfaceHover/30">${alt}</span>` : ""
    }</span>`;
  });

  // Links: [text](url)
  out = out.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, text, href) => {
    const safe = safeUrl(href);
    // A refused link keeps its text: dropping it would hide content, and
    // rendering it as a link would be the thing being prevented.
    if (safe === null) {
      return `<span class="text-textPrimary/90">${text}</span>`;
    }
    return `<a href="${safe}" target="_blank" rel="noopener noreferrer" class="text-accent underline underline-offset-2 hover:text-accent/80 transition-colors">${text} ↗</a>`;
  });

  // Bold / Italic
  out = out.replace(/\*\*([^*]+)\*\*/g, '<strong class="font-bold text-textPrimary">$1</strong>');
  out = out.replace(/__([^_]+)__/g, '<strong class="font-bold text-textPrimary">$1</strong>');
  out = out.replace(/\*([^*]+)\*/g, '<em class="italic text-textPrimary/90">$1</em>');
  out = out.replace(/_([^_]+)_/g, '<em class="italic text-textPrimary/90">$1</em>');

  return out;
}
