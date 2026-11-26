#!/usr/bin/env node
// Phase 1 of the GitPulse -> Waylode rename: the product name where a person
// reads it, and nowhere else.
//
//   node scripts/brand-rename.mjs            report what would change (exit 1 if anything would)
//   node scripts/brand-rename.mjs --write    apply it
//   node scripts/brand-rename.mjs --kept     also list the exact "GitPulse" values left alone
//
// The rename is a script rather than a diff so it can be run again on a fresh
// main: branches merged after it bring the old name back in places, and the
// contract test (scripts/brand-rename.contract.test.ts) fails until it is run.
//
// What it renames:
//   - text inside string literals in the app's TypeScript, Svelte and Rust;
//   - Svelte and HTML markup text outside comments;
//   - Markdown prose in the user docs, outside code blocks and code spans.
// What it never touches (each is a name something else looks up, so it moves
// with the identity migration, or is agent-facing and moves with the tool names):
//   - comments, identifiers (GitPulseTheme), paths (/GitPulse/, GitPulse.app);
//   - a string whose whole value is "GitPulse" (repository names in fixtures,
//     the notification app id, Application Support and log directory names);
//   - the bundle productName and identifier, binaries, MCP tool names, URIs,
//     environment variables, git config keys, saved settings keys;
//   - agent-facing text: the MCP server, hooks, the agent harness, plugins,
//     skills, CLAUDE.md and AGENTS.md;
//   - vendored crates, contracts, fixtures, test files, CHANGELOG history.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const OLD = "GitPulse";
export const NEW = "Waylode";

/** A standalone product-name token: not part of an identifier, path, domain or file name. */
const TOKEN = /(?<![A-Za-z0-9_/\\.@$-])GitPulse(?![A-Za-z0-9_/-])(?!\.[A-Za-z0-9])/g;

const TEST_FILE = /(\.test\.|\.spec\.|_tests?\.rs$|\/tests?\.rs$|\/tests?\/|\/test_support|\/fixtures?\/)/;

/**
 * Rust files that are agent-facing (Phase 2) or whose strings are a format
 * another process reads back.
 */
const RUST_EXCLUDED = [
  "src-tauri/src/mcp/",
  "src-tauri/src/hooks/",
  "src-tauri/src/harness/",
  "src-tauri/src/bin/",
  "src-tauri/src/insights/",
  "src-tauri/src/ledger/",
  "src-tauri/src/agent_bus",
  "src-tauri/src/agent_connect",
  "src-tauri/src/agent_mailbox",
  "src-tauri/src/engine/agent_bus",
  "src-tauri/src/engine/process/shared_budget.rs",
  "src-tauri/src/terminal/shell_integration.rs",
  "src-tauri/src/desktop/launch_agent.rs",
  "src-tauri/src/desktop/identity_migration.rs",
];

/** Frontend files whose text is written for an agent to read (Phase 2). */
const AGENT_FACING_TS = /(agentPrompt|agentConnect|collaboration|selectionContent|sendToAgent|taskCompose|handoff)[^/]*\.ts$/;

/**
 * A string holding one of these is a marker or a value something reads back
 * (exclude-file markers, the LaunchAgent ownership comment, backup headers,
 * the git identity of the app's own commits, the prompt typed into agents).
 */
const KEPT_SHAPES = [
  /user\.name=GitPulse/,
  /# GitPulse:/,
  /GitPulse Deadbranch backup/,
  /<!-- GitPulse /,
  /Managed by GitPulse/,
  /GitPulse peer inbox/,
];

/**
 * Exact standalone `"GitPulse"` string literals kept on purpose: OS profile,
 * log, and cache directory names, synthetic test fixtures, legacy identity
 * migration probes, and the sentence-case proper-noun table (which also keeps
 * `"GitPulse"` alongside `"Waylode"`).
 */
const KEPT_STANDALONE_PATHS = new Set([
  "src-tauri/src/alerts/stress.rs",
  "src-tauri/src/logging.rs",
  "src-tauri/src/tool_config.rs",
  "src-tauri/src/workbench/verification_driver_measure.rs",
  "src/lib/ui/copy/sentenceCase.ts",
  "src-tauri/src/desktop/identity_migration.rs",
  "src-tauri/src/desktop/launch.rs",
  "src-tauri/src/desktop/launch_agent.rs",
  "src-tauri/src/daemon_lock.rs",
  "src-tauri/src/agent_connect.rs",
]);

/**
 * Rust items in mixed UI/agent files whose strings are agent-facing prompts or
 * shell hook wrappers and must not be rewritten as UI copy in Phase 1.
 * @type {Record<string, Set<string>>}
 */
const RUST_EXCLUDED_ITEMS = {
  "src-tauri/src/workbench/terminal_command.rs": new Set(["arguments", "COMPLETION"]),
  "src-tauri/src/workbench/agy_plugin.rs": new Set([
    "build_mcp_wrapper",
    "build_hook_wrapper",
    "rewrite_hooks_json",
  ]),
};

/** Markdown that is history, legal text, rename plans, or agent instructions. */
const MD_EXCLUDED = new Set([
  "CHANGELOG.md",
  "TRADEMARKS.md",
  "CLAUDE.md",
  "AGENTS.md",
  "NOTICE.md",
  "docs/WAYLODE_RENAME_PLAN.md",
]);

/** @typedef {[number, number]} Range  half-open [start, end) offsets */
/** @typedef {"ts" | "rust" | "svelte" | "html" | "md"} Kind */
/** @typedef {{ ranges: Range[], kept: number[] }} Scan */

/**
 * Which kind of scan a tracked path gets, or null to leave it alone.
 * @param {string} path
 * @returns {Kind | null}
 */
export function kindOf(path) {
  if (TEST_FILE.test(path)) return null;
  if (path === "index.html") return "html";
  if (path.startsWith("src/")) {
    if (path.endsWith(".svelte")) return "svelte";
    if (path.endsWith(".ts")) return AGENT_FACING_TS.test(path) ? null : "ts";
    return null;
  }
  if (path.startsWith("src-tauri/src/") && path.endsWith(".rs")) {
    return RUST_EXCLUDED.some((dir) => path.startsWith(dir)) ? null : "rust";
  }
  if (path.endsWith(".md")) {
    if (MD_EXCLUDED.has(path)) return null;
    // Release notes and published posts record what shipped under the old name.
    if (/^docs\/(RELEASE_NOTES_|launch\/social-)/.test(path)) return null;
    if (["README.md", "CONTRIBUTING.md", "SUPPORT.md", "GOVERNANCE.md", "CODE_OF_CONDUCT.md"].includes(path)) return "md";
    if (path.startsWith("docs/") || path.startsWith("wiki/")) return "md";
  }
  return null;
}

/**
 * Ranges of `text` where the product name is reader-facing, plus the exact
 * "GitPulse" string values found (kept, by rule).
 * @param {string} text
 * @param {Kind | null} kind
 * @returns {Scan}
 */
export function readerRanges(text, kind, path = "") {
  switch (kind) {
    case "ts":
      return codeStrings(text, 0, text.length, "ts", path);
    case "rust":
      return codeStrings(text, 0, text.length, "rust", path);
    case "svelte":
      return svelteRanges(text, path);
    case "html":
      return { ranges: outsideHtmlComments(text, 0, text.length), kept: [] };
    case "md":
      return { ranges: markdownProse(text), kept: [] };
    default:
      return { ranges: [], kept: [] };
  }
}

/**
 * String-literal bodies in TS or Rust source between `from` and `to`.
 * @param {string} text
 * @param {number} from
 * @param {number} to
 * @param {"ts" | "rust"} lang
 * @param {string} path
 * @returns {Scan}
 */
function codeStrings(text, from, to, lang, path = "") {
  /** @type {Range[]} */
  const ranges = [];
  /** @type {number[]} */
  const kept = [];
  /** @type {(start: number, end: number) => void} */
  const push = (start, end) => {
    const body = text.slice(start, end);
    if (!body.includes(OLD)) return;
    if ((body === OLD && KEPT_STANDALONE_PATHS.has(path)) || KEPT_SHAPES.some((shape) => shape.test(body))) {
      kept.push(start);
    } else {
      ranges.push([start, end]);
    }
  };
  if (lang === "rust") {
    // Inline test items and agent-facing prompt/hook functions are skipped
    // while keeping the rest of the file's user-facing strings eligible.
    const skipped = mergeRanges([
      ...rustTestBlocks(text, from, to),
      ...rustExcludedItemRanges(text, from, to, RUST_EXCLUDED_ITEMS[path]),
    ]);
    let at = from;
    for (const [start, end] of skipped) {
      if (at < start) lexCode(text, at, start, lang, push, false);
      at = Math.max(at, end);
    }
    if (at < to) lexCode(text, at, to, lang, push, false);
  } else {
    lexCode(text, from, to, lang, push, false);
  }
  return { ranges, kept };
}

/**
 * Sort and merge overlapping half-open ranges.
 * @param {Range[]} ranges
 * @returns {Range[]}
 */
function mergeRanges(ranges) {
  if (ranges.length === 0) return [];
  const sorted = [...ranges].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  /** @type {Range[]} */
  const merged = [sorted[0]];
  for (let i = 1; i < sorted.length; i++) {
    const [s, e] = sorted[i];
    const last = merged[merged.length - 1];
    if (s <= last[1]) last[1] = Math.max(last[1], e);
    else merged.push([s, e]);
  }
  return merged;
}

/**
 * Find the end offset of a Rust item starting at `pos` (either `{ ... }` or `;`),
 * skipping strings, char literals, and comments so inner braces never confuse it.
 * @param {string} text
 * @param {number} pos
 * @param {number} to
 * @returns {number}
 */
function rustItemEnd(text, pos, to) {
  let i = pos;
  let braceDepth = 0;
  let parenDepth = 0;
  let bracketDepth = 0;
  let sawBrace = false;
  while (i < to) {
    const c = text[i];
    const n = text[i + 1];
    if (c === "/" && n === "/") {
      const end = text.indexOf("\n", i);
      i = end < 0 || end > to ? to : end;
    } else if (c === "/" && n === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 || end > to ? to : end + 2;
    } else if (c === "r" && (n === '"' || n === "#") && !/[A-Za-z0-9_]/.test(text[i - 1] ?? "")) {
      const m = /^r(#*)"/.exec(text.slice(i, i + 40));
      if (!m) {
        i++;
        continue;
      }
      const close = `"${m[1]}`;
      const start = i + m[0].length;
      const end = text.indexOf(close, start);
      i = end < 0 || end > to ? to : end + close.length;
    } else if (c === "'") {
      const m = /^'(\\u\{[0-9A-Fa-f]{1,6}\}|\\.|[^\\'\n])'/.exec(text.slice(i, i + 12));
      i += m ? m[0].length : 1;
    } else if (c === '"') {
      let j = i + 1;
      while (j < to && text[j] !== '"' && text[j] !== "\n") {
        if (text[j] === "\\") j++;
        j++;
      }
      i = j + 1;
    } else if (c === "(") {
      parenDepth++;
      i++;
    } else if (c === ")") {
      if (parenDepth > 0) parenDepth--;
      i++;
    } else if (c === "[") {
      bracketDepth++;
      i++;
    } else if (c === "]") {
      if (bracketDepth > 0) bracketDepth--;
      i++;
    } else if (c === "{" && parenDepth === 0 && bracketDepth === 0) {
      braceDepth++;
      sawBrace = true;
      i++;
    } else if (c === "}" && parenDepth === 0 && bracketDepth === 0) {
      if (braceDepth > 0) braceDepth--;
      i++;
      if (braceDepth === 0 && sawBrace) return i;
    } else if (c === ";" && braceDepth === 0 && parenDepth === 0 && bracketDepth === 0) {
      return i + 1;
    } else {
      i++;
    }
  }
  return to;
}

/**
 * Skip stacked `#[...]` attributes and whitespace after `pos`, returning the
 * index of the attributed item.
 * @param {string} text
 * @param {number} pos
 * @param {number} to
 * @returns {number}
 */
function skipRustAttributes(text, pos, to) {
  let i = pos;
  while (i < to) {
    while (i < to && /\s/.test(text[i])) i++;
    if (text[i] === "/" && text[i + 1] === "/") {
      const nl = text.indexOf("\n", i);
      i = nl < 0 ? to : nl + 1;
      continue;
    }
    if (text[i] === "#" && text[i + 1] === "[") {
      let depth = 0;
      while (i < to) {
        if (text[i] === '"') {
          i++;
          while (i < to && text[i] !== '"') {
            if (text[i] === "\\") i++;
            i++;
          }
        } else if (text[i] === "[") {
          depth++;
        } else if (text[i] === "]") {
          depth--;
          if (depth === 0) {
            i++;
            break;
          }
        }
        i++;
      }
      continue;
    }
    break;
  }
  return i;
}

/**
 * Inline `#[cfg(test)]` / `#[cfg(all(test, ...))]` items (modules, test fns,
 * helpers, constants) as [start, end) ranges.
 * @param {string} text
 * @param {number} from
 * @param {number} to
 * @returns {Range[]}
 */
function rustTestBlocks(text, from, to) {
  /** @type {Range[]} */
  const out = [];
  const head = /#\[cfg\((?:test|all\([^)]*\btest\b[^)]*\))\)\]/g;
  head.lastIndex = from;
  for (let m = head.exec(text); m && m.index < to; m = head.exec(text)) {
    const itemStart = skipRustAttributes(text, m.index + m[0].length, to);
    if (
      !/^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:mod|fn|struct|enum|impl|trait|const|static|type|use|macro_rules!)\b/.test(
        text.slice(itemStart, itemStart + 80),
      )
    ) {
      continue;
    }
    const end = rustItemEnd(text, itemStart, to);
    out.push([m.index, end]);
    head.lastIndex = end;
  }
  return out;
}

/**
 * Specific Rust functions/constants excluded by name (agent-facing prompts/hooks).
 * @param {string} text
 * @param {number} from
 * @param {number} to
 * @param {Set<string> | undefined} names
 * @returns {Range[]}
 */
function rustExcludedItemRanges(text, from, to, names) {
  if (!names || names.size === 0) return [];
  /** @type {Range[]} */
  const out = [];
  const item = /(?:^|\n)[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:fn|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)\b/g;
  item.lastIndex = from;
  for (let m = item.exec(text); m && m.index < to; m = item.exec(text)) {
    if (!names.has(m[1])) continue;
    const start = m.index + (m[0].startsWith("\n") ? 1 : 0);
    const end = rustItemEnd(text, start, to);
    out.push([start, end]);
    item.lastIndex = end;
  }
  return out;
}

/**
 * The last significant character before `i`, to tell a regex `/` from division.
 * @param {string} text
 * @param {number} from
 * @param {number} i
 */
function regexCanStart(text, from, i) {
  let j = i - 1;
  while (j >= from && /\s/.test(text[j])) j--;
  if (j < from) return true;
  const c = text[j];
  if ("(,=:[!&|?{};+-*%<>~^".includes(c)) return true;
  const word = /([A-Za-z_$]+)$/.exec(text.slice(Math.max(from, j - 10), j + 1));
  return word !== null && ["return", "typeof", "case", "in", "of", "delete", "void", "throw", "new"].includes(word[1]);
}

/**
 * Walk code from `from`, reporting string bodies through `push`. In TS a
 * template literal's `${...}` is code again (so nested templates and comments
 * inside it are handled), and a regex literal is skipped. When `untilBrace` is
 * set, stops after the `}` that closes the current `${` and returns its index.
 * @param {string} text
 * @param {number} from
 * @param {number} to
 * @param {"ts" | "rust"} lang
 * @param {(start: number, end: number) => void} push
 * @param {boolean} untilBrace
 * @returns {number}
 */
function lexCode(text, from, to, lang, push, untilBrace) {
  let i = from;
  let depth = 0;
  while (i < to) {
    const c = text[i];
    const n = text[i + 1];
    if (c === "/" && n === "/") {
      const end = text.indexOf("\n", i);
      i = end < 0 || end > to ? to : end;
    } else if (c === "/" && n === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 || end > to ? to : end + 2;
    } else if (lang === "rust" && c === "r" && (n === '"' || n === "#") && !/[A-Za-z0-9_]/.test(text[i - 1] ?? "")) {
      const m = /^r(#*)"/.exec(text.slice(i, i + 40));
      if (!m) {
        i++;
        continue;
      }
      const close = `"${m[1]}`;
      const start = i + m[0].length;
      const end = text.indexOf(close, start);
      const stop = end < 0 || end > to ? to : end;
      push(start, stop);
      i = stop + close.length;
    } else if (lang === "rust" && c === "'") {
      // A char literal ('"', '\'', '\u{1F600}'), or a lifetime / label.
      const m = /^'(\\u\{[0-9A-Fa-f]{1,6}\}|\\.|[^\\'\n])'/.exec(text.slice(i, i + 12));
      i += m ? m[0].length : 1;
    } else if (lang === "ts" && c === "`") {
      // Template literal: text segments are strings, `${...}` is code.
      let seg = i + 1;
      let j = seg;
      while (j < to && text[j] !== "`") {
        if (text[j] === "\\") {
          j += 2;
        } else if (text[j] === "$" && text[j + 1] === "{") {
          push(seg, j);
          j = lexCode(text, j + 2, to, lang, push, true) + 1;
          seg = j;
        } else {
          j++;
        }
      }
      push(seg, Math.min(j, to));
      i = j + 1;
    } else if (c === '"' || (lang === "ts" && c === "'")) {
      const start = i + 1;
      let j = start;
      while (j < to && text[j] !== c && text[j] !== "\n") {
        if (text[j] === "\\") j++;
        j++;
      }
      push(start, Math.min(j, to));
      i = j + 1;
    } else if (lang === "ts" && c === "/" && regexCanStart(text, from, i)) {
      let j = i + 1;
      let inClass = false;
      while (j < to && text[j] !== "\n") {
        if (text[j] === "\\") j++;
        else if (text[j] === "[") inClass = true;
        else if (text[j] === "]") inClass = false;
        else if (text[j] === "/" && !inClass) break;
        j++;
      }
      i = j + 1;
    } else if (untilBrace && c === "{") {
      depth++;
      i++;
    } else if (untilBrace && c === "}") {
      if (depth === 0) return i;
      depth--;
      i++;
    } else {
      i++;
    }
  }
  return to;
}

/**
 * Everything between from and to that is not inside an HTML comment.
 * @param {string} text
 * @param {number} from
 * @param {number} to
 * @returns {Range[]}
 */
function outsideHtmlComments(text, from, to) {
  /** @type {Range[]} */
  const ranges = [];
  let i = from;
  while (i < to) {
    const open = text.indexOf("<!--", i);
    if (open < 0 || open >= to) {
      ranges.push([i, to]);
      break;
    }
    ranges.push([i, open]);
    const close = text.indexOf("-->", open + 4);
    i = close < 0 ? to : close + 3;
  }
  return ranges;
}

/**
 * Svelte: script blocks follow TS rules, styles are skipped, markup is text outside comments.
 * @param {string} text
 * @returns {Scan}
 */
function svelteRanges(text, path = "") {
  /** @type {Range[]} */
  const ranges = [];
  /** @type {number[]} */
  const kept = [];
  const block = /<(script|style)\b[^>]*>([\s\S]*?)<\/\1>/g;
  let at = 0;
  for (const m of text.matchAll(block)) {
    ranges.push(...outsideHtmlComments(text, at, m.index));
    if (m[1] === "script") {
      const bodyStart = m.index + m[0].indexOf(">") + 1;
      const inner = codeStrings(text, bodyStart, bodyStart + m[2].length, "ts", path);
      ranges.push(...inner.ranges);
      kept.push(...inner.kept);
    }
    at = m.index + m[0].length;
  }
  ranges.push(...outsideHtmlComments(text, at, text.length));
  return { ranges, kept };
}

/**
 * Markdown prose: not fenced code, not inline code spans, not HTML comments.
 * @param {string} text
 * @returns {Range[]}
 */
function markdownProse(text) {
  /** @type {Range[]} */
  const ranges = [];
  let offset = 0;
  /** @type {string | null} */
  let fence = null;
  for (const line of text.split("\n")) {
    const end = offset + line.length;
    const marker = /^\s*(```+|~~~+)/.exec(line);
    if (fence) {
      if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length) fence = null;
    } else if (marker) {
      fence = marker[1];
    } else {
      let i = offset;
      for (const span of line.matchAll(/(`+)[\s\S]*?\1/g)) {
        ranges.push([i, offset + span.index]);
        i = offset + span.index + span[0].length;
      }
      ranges.push([i, end]);
    }
    offset = end + 1;
  }
  const comments = outsideHtmlComments(text, 0, text.length);
  return intersect(ranges, comments);
}

/**
 * @param {Range[]} a
 * @param {Range[]} b
 * @returns {Range[]}
 */
function intersect(a, b) {
  /** @type {Range[]} */
  const out = [];
  for (const [s1, e1] of a) {
    for (const [s2, e2] of b) {
      const s = Math.max(s1, s2);
      const e = Math.min(e1, e2);
      if (s < e) out.push([s, e]);
    }
  }
  return out;
}

/**
 * The text with the product name replaced inside `ranges`, and how many were replaced.
 * @param {string} text
 * @param {Range[]} ranges
 * @returns {{ text: string, count: number }}
 */
export function renameIn(text, ranges) {
  let count = 0;
  let out = "";
  let at = 0;
  for (const [s, e] of [...ranges].sort((x, y) => x[0] - y[0])) {
    if (s < at) continue;
    out += text.slice(at, s);
    out += text.slice(s, e).replace(TOKEN, () => {
      count++;
      return NEW;
    });
    at = e;
  }
  return { text: out + text.slice(at), count };
}

/**
 * Exact phrases in files the scan leaves alone (tests, configs) that quote the
 * renamed copy, and so must change with it. Each phrase is replaced with its
 * Waylode form. Applying them is idempotent: a phrase already in its new form
 * is fine; a phrase in neither form is stale and fails the run, so a moved or
 * reworded assertion is noticed instead of silently left quoting old copy.
 * Fixtures (repository names, git identities, paths) are deliberately absent.
 * @type {Record<string, string[]>}
 */
export const EXPECTATIONS = {
  "src-tauri/tauri.conf.json": ['"title": "GitPulse",'],
  "src-tauri/tauri.windows.conf.json": ['"title": "GitPulse",'],
  "src-tauri/tauri.macos.conf.json": ['"title": "GitPulse",'],
  "scripts/architecture-docs-contract.test.ts": ["must say GitPulse uses Manvi", "/GitPulse[\\s*]*uses[\\s*]*Manvi/i"],
  "src/lib/agents/plane.attribution.test.ts": ['toBe("GitPulse task")'],
  "src/lib/coverage/adversarial.stress.test.ts": ['toContain("GitPulse clipped this draft")'],
  "src/lib/coverage/report.test.ts": [
    "[GitPulse context clipped",
    'toContain("GitPulse clipped this draft")',
    "[GitPulse clipped this draft to stay",
  ],
  "src/lib/diagnostics/diagnostics.test.ts": ["by GitPulse 0.0.3"],
  "src/lib/firebase/diagnostics.test.ts": ['"GitPulse: 1.99.0"'],
  "src/lib/files/markdownLinks.test.ts": ['"GitPulse opens"'],
  "src/lib/github/prActions.test.ts": ['"GitPulse does not push"'],
  "src/lib/health/hostLoadInputs.test.ts": ["Slow GitPulse's background work"],
  "src/lib/components/FleetView.test.ts": ["Claude Code, GitPulse task."],
  "src/lib/components/GlobalCleaner.test.ts": ['toContain("not cleaned by GitPulse")'],
  "src/lib/components/HealthPanel.test.ts": ["checked when GitPulse starts"],
  "src/lib/components/KeepTerminalsSetting.test.ts": ["when GitPulse quits"],
  "src/lib/components/ManviConnection.test.ts": ["GitPulse will not treat a timed-out"],
  "src/lib/components/NativeNotificationBridgeView.test.ts": [">GitPulse activity<"],
  "src/lib/components/ReleasePublish.test.ts": ["GitPulse will still require"],
  "src/lib/components/SessionAlertSettings.test.ts": ["no reply from GitPulse in time"],
  "src/lib/components/TrustExtensionBanner.test.ts": ["/before GitPulse covered worktrees/"],
  "src/lib/components/WorkspaceEditor.test.ts": ['"Open in GitPulse"'],
  "src/lib/components/WorktreesPanel.test.ts": ["'GitPulse task worktree'", "(GitPulse only)"],
  "src/lib/components/adv_w3c_SessionAlertSettings.render.test.ts": ["no reply from GitPulse in time"],
  "src/lib/provenance/badge.test.ts": ["GitPulse does not ingest"],
  "src/lib/repos/repositoryTrust.test.ts": ["/before GitPulse covered worktrees/"],
  "src/lib/repos/watchState.test.ts": ['"outside GitPulse"'],
  "src/lib/work/agentWorktree.test.ts": ['toBe("GitPulse task")'],
  "src/lib/terminal/legacySessions.test.ts": ["previous GitPulse version's"],
  "src/lib/terminal/sessionContext.test.ts": ["GitPulse couldn't tell whether"],
  "src/lib/components/health/HostLoadSection.test.ts": ["available to GitPulse"],
  "src/lib/stores/__tests__/harnessStore.test.ts": ["inside GitPulse. Details"],
  "src-tauri/src/workbench/profile_owner_tests.rs": ['contains("restart GitPulse")'],
  "src-tauri/src/ai/tests/manvi_route_tests.rs": ["Update Manvi to run GitPulse's AI features through it."],
  "src-tauri/src/workbench/reconcile_tests.rs": ['r.contains("Another running GitPulse")'],
  "src-tauri/src/hooks/lease_wrapper_tests.rs": ['contains("not at this GitPulse")'],
  "src-tauri/src/workbench/send_to_agent/tests.rs": ['.contains("No GitPulse hook is waiting")'],
  "src-tauri/tests/repository_trust.rs": ["approved before GitPulse covered worktrees"],
  "src-tauri/src/workbench/verification_driver_tests.rs": ['contains("GitPulse closed")'],
  "src-tauri/src/storage/repo_tuning/advice_tests.rs": ['contains("GitPulse\'s own")'],
  "src-tauri/src/engine/git_writer_commit_graph_adv_tests.rs": ['message.contains("GitPulse never removes")'],
  "src-tauri/src/tool_install/mod.rs": ["too old for this GitPulse"],
  "src-tauri/src/devmap/cli.rs": ['bounded.contains("GitPulse shortened this warning")'],
  "src-tauri/src/secrets/mod.rs": ["kingfisher was deferred: GitPulse was under load"],
  "src-tauri/src/workbench/notifications.rs": ['assert_eq!(ACTIVITY_HEADING, "GitPulse activity")'],
  "src-tauri/src/commands/export.rs": ["GitPulse isn't allowed to write theme.json in that folder."],
  "src-tauri/src/sysload/procscan.rs": ['refused.contains("not one GitPulse will terminate")'],
  "src-tauri/src/desktop/tray.rs": ['label: "Open GitPulse".into()', 'label: "Quit GitPulse".into()'],
};

/**
 * `text` with this file's expectation phrases in their new form.
 * @param {string} path
 * @param {string} text
 * @returns {{ text: string, count: number, stale: string[] }}
 */
export function applyExpectations(path, text) {
  let count = 0;
  /** @type {string[]} */
  const stale = [];
  for (const phrase of EXPECTATIONS[path] ?? []) {
    const renamed = phrase.replaceAll(OLD, NEW);
    const n = text.split(phrase).length - 1;
    if (n > 0) {
      text = text.replaceAll(phrase, renamed);
      count += n;
    } else if (!text.includes(renamed)) {
      stale.push(phrase);
    }
  }
  return { text, count, stale };
}

export function trackedFiles(root = process.cwd()) {
  return execFileSync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8", maxBuffer: 1 << 28 })
    .split("\0")
    .filter(Boolean);
}

/**
 * Every file the rename would change, with counts, every exact value it keeps,
 * and every expectation phrase found in neither its old nor its new form.
 * @returns {{ changes: { path: string, count: number, text: string }[], kept: { path: string, line: number }[], stale: { path: string, phrase: string }[] }}
 */
export function plan(root = process.cwd()) {
  /** @type {{ path: string, count: number, text: string }[]} */
  const changes = [];
  /** @type {{ path: string, line: number }[]} */
  const kept = [];
  /** @type {{ path: string, phrase: string }[]} */
  const stale = [];
  const tracked = new Set(trackedFiles(root));
  for (const path of Object.keys(EXPECTATIONS)) {
    if (!tracked.has(path)) stale.push({ path, phrase: "(file is not tracked)" });
  }
  for (const path of tracked) {
    const kind = kindOf(path);
    if (!kind && !(path in EXPECTATIONS)) continue;
    let text;
    try {
      text = readFileSync(`${root}/${path}`, "utf8");
    } catch {
      continue;
    }
    let count = 0;
    if (kind && text.includes(OLD)) {
      const scan = readerRanges(text, kind, path);
      for (const at of scan.kept) kept.push({ path, line: text.slice(0, at).split("\n").length });
      const renamed = renameIn(text, scan.ranges);
      text = renamed.text;
      count += renamed.count;
    }
    if (path in EXPECTATIONS) {
      const expected = applyExpectations(path, text);
      text = expected.text;
      count += expected.count;
      for (const phrase of expected.stale) stale.push({ path, phrase });
    }
    if (count > 0) changes.push({ path, count, text });
  }
  return { changes, kept, stale };
}

function main() {
  const args = new Set(process.argv.slice(2));
  const { changes, kept, stale } = plan();
  const total = changes.reduce((n, c) => n + c.count, 0);
  if (stale.length > 0) {
    for (const s of stale) console.error(`stale expectation in ${s.path}: ${s.phrase}`);
    console.error(`${stale.length} expectation(s) match neither the old nor the new wording; fix EXPECTATIONS first`);
    process.exitCode = 1;
    return;
  }
  if (args.has("--write")) {
    for (const c of changes) writeFileSync(c.path, c.text);
    // The new name is shorter, so a renamed string can now fit on fewer
    // lines; rustfmt (the same check `cargo fmt -- --check` runs in ci:local)
    // settles the layout so the rename leaves the format check green.
    const rust = changes.map((c) => c.path).filter((p) => p.endsWith(".rs"));
    if (rust.length > 0) execFileSync("rustfmt", ["--edition", "2021", ...rust], { stdio: "inherit" });
    console.log(`renamed ${total} occurrence(s) of ${OLD} in ${changes.length} file(s)`);
  } else {
    for (const c of changes) console.log(`${String(c.count).padStart(4)}  ${c.path}`);
    console.log(`${total} occurrence(s) of ${OLD} in ${changes.length} file(s) would become ${NEW}`);
  }
  if (args.has("--kept")) for (const k of kept) console.log(`kept "${OLD}"  ${k.path}:${k.line}`);
  console.log(`${kept.length} exact "${OLD}" value(s) kept by rule`);
  if (!args.has("--write") && total > 0) process.exitCode = 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();
