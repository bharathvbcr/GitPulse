import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Platform-specific words reach the reader from ONE module.
 *
 * Four surfaces shipped macOS vocabulary to every host before this existed: the
 * desktop-notification panel ("macOS permission", "this Mac's local time"), the
 * Dock toggle, the build cleaner telling Windows users to install a macOS
 * application bundle, and the launch-at-login note naming a LaunchAgent. Each
 * was a separate string written in a separate component, and each was invisible
 * to everyone on a Mac — which is everyone who developed it.
 *
 * The same shape produced the shortcut bugs: the command palette rewrote ⌘ and
 * ⇧ by hand and so missed ⌃ entirely (the terminal dock's `⌃\`` reached Windows
 * as a Mac glyph), while the shortcuts dialog rewrote nothing at all.
 *
 * So this pins the seam rather than the four fixes: platform words and glyph
 * rewriting live in `src/lib/ui/platformCopy.ts`, and a component asks it.
 */
const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SCANNED_DIRS = [
  "src/lib/components",
  "src/lib/desktop",
  "src/lib/views",
  "src/lib/palette",
  "src/lib/ui",
  "src/lib/terminal",
];
/** Files outside those trees that still render to the reader. */
const SCANNED_FILES = ["src/App.svelte"];

/** The one module allowed to spell these out, plus its own test. */
const OWNERS = new Set(["src/lib/ui/platformCopy.ts", "src/lib/ui/platformCopy.test.ts"]);

/**
 * Words that name one platform's furniture.
 *
 * "Dock" is deliberately absent: GitPulse's own terminal dock is unrelated
 * furniture with a legitimate claim on the word, and guarding it would train
 * people to add suppressions. The Dock *setting* is gated by capability instead,
 * which is the stronger guarantee anyway.
 */
const PLATFORM_WORDS = /\b(macOS|Finder|LaunchAgent|File Explorer|Windows Settings)\b|\bthis Mac\b/;

/** macOS modifier glyphs, which must be mapped rather than printed raw. */
const GLYPHS = /[⌘⌥⌃]/;

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(join(ROOT, dir), { withFileTypes: true })) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) walk(path, out);
    else if (/\.(svelte|ts)$/.test(entry.name) && !/\.(test|spec)\.ts$/.test(entry.name)) {
      out.push(path);
    }
  }
  return out;
}

/**
 * Calls into the mapper, which are the CORRECT way to write a glyph.
 *
 * Removed before scanning: `shortcutTextLabel("⌘B", os)` contains a glyph, but
 * as an argument being translated, not as text heading for a reader. Without
 * this the guard flags exactly the fix it is asking for.
 */
const MAPPER_CALL = /\b(?:shortcutTextLabel|shortcutKeyLabels?|platformChord)\([^)]*\)/g;

/** Double-quoted, single-quoted and template string bodies on one line. */
function stringLiterals(line: string): string[] {
  return [...line.matchAll(/"([^"\\]*(?:\\.[^"\\]*)*)"|'([^'\\]*(?:\\.[^'\\]*)*)'|`([^`\\]*)`/g)]
    .map((match) => match[1] ?? match[2] ?? match[3] ?? "")
    .filter((text) => text.length > 0);
}

/** Comment lines, where naming a platform is explanation and not output. */
function isComment(line: string): boolean {
  const trimmed = line.trim();
  return (
    trimmed.startsWith("//") ||
    trimmed.startsWith("*") ||
    trimmed.startsWith("/*") ||
    trimmed.startsWith("<!--")
  );
}

interface Hit {
  file: string;
  line: number;
  text: string;
}

function scannedFiles(): string[] {
  return [...SCANNED_DIRS.flatMap((dir) => walk(dir)), ...SCANNED_FILES];
}

/**
 * The parts of a line that can reach a reader.
 *
 * Precise per file type, because the two are not alike:
 *
 *   - In `.ts`, anything visible is inside a string literal. Scanning whole
 *     lines there flags identifiers — `const macOS = isMacOS()` is code, not
 *     copy — and training people to ignore a guard is worse than not having it.
 *   - In a `.svelte` TEMPLATE, the visible text is mostly unquoted: a bare
 *     `<span class="gp-keycap">⌘F</span>` is as visible as any label. Scanning
 *     only literals there is the hole that let three components keep raw
 *     glyphs while this guard reported green.
 *
 * So: literals inside `<script>` and in `.ts`, whole lines in the template.
 */
function visibleText(file: string, line: string, inTemplate: boolean): string[] {
  return file.endsWith(".svelte") && inTemplate ? [line] : stringLiterals(line);
}

function scan(pattern: RegExp, stripMapperCalls = true): Hit[] {
  const hits: Hit[] = [];
  for (const file of scannedFiles()) {
    if (OWNERS.has(file)) continue;
    const source = readFileSync(join(ROOT, file), "utf8");
    let inTemplate = !file.endsWith(".svelte");
    source.split("\n").forEach((line, index) => {
      if (line.includes("</script>")) {
        inTemplate = true;
        return;
      }
      if (isComment(line)) return;
      const mapped = stripMapperCalls ? line.replace(MAPPER_CALL, "") : line;
      for (const candidate of visibleText(file, mapped, inTemplate)) {
        if (pattern.test(candidate)) {
          hits.push({ file, line: index + 1, text: candidate.trim().slice(0, 90) });
        }
      }
    });
  }
  return hits;
}

function describeHits(hits: readonly Hit[]): string {
  return hits.map((hit) => `${hit.file}:${hit.line}  ${hit.text}`).join("\n");
}

describe("platform vocabulary has one owner", () => {
  it("keeps platform-specific words out of anything a reader sees", () => {
    const hits = scan(PLATFORM_WORDS);
    expect(
      hits,
      "these strings name one platform's furniture to every host; take the word " +
        "from src/lib/ui/platformCopy.ts instead:\n" + describeHits(hits),
    ).toEqual([]);
  });

  /**
   * The guard must be able to fail, or it is decoration. If this stops matching
   * the owner module, the scanner has drifted away from what it claims to check.
   */
  it("would catch the wording it exists to prevent", () => {
    expect(PLATFORM_WORDS.test("macOS permission: authorized")).toBe(true);
    expect(PLATFORM_WORDS.test("Quiet hours in this Mac's local time")).toBe(true);
    expect(PLATFORM_WORDS.test("Uses a per-user LaunchAgent")).toBe(true);
    expect(PLATFORM_WORDS.test("available from an installed macOS application bundle")).toBe(true);
    // And must not fire on wording that is already platform-neutral.
    expect(PLATFORM_WORDS.test("Quiet hours in this computer's local time")).toBe(false);
    expect(PLATFORM_WORDS.test("Toggle the terminal dock")).toBe(false);
  });
});

describe("modifier glyphs have one mapper", () => {
  /**
   * `ShortcutsModal` and `palette/catalog` are the two tables that AUTHOR
   * shortcuts in macOS notation; both render through `shortcutKeyLabels` /
   * `shortcutTextLabel`. Everywhere else a raw glyph is a string heading for a
   * reader who may not be on a Mac.
   */
  const TABLES = new Set([
    "src/lib/components/ShortcutsModal.svelte",
    // The canonical chord table. It is data, not output: `view-menu-contract`
    // holds the native menu to exactly these strings, and every render site
    // (the cheat sheet, the destination tooltips) maps them first.
    "src/lib/views/viewShortcuts.ts",
    // The palette's own command table, rendered through `shortcutTextLabel`.
    "src/lib/palette/catalog.ts",
    // Authors the coach-mark chord, which `CoachMark` translates for every
    // caller — pinned by CoachMark.platform.test.ts, which is the actual
    // guarantee here; this entry only stops the guard flagging the input.
    "src/App.svelte",
  ]);

  /**
   * Whole lines, not just quoted strings.
   *
   * The first version of this guard scanned string literals only and reported
   * green while `<span class="gp-keycap">⌘F</span>` sat in three components:
   * a Svelte text node is as visible as any label and carries no quotes. In a
   * `.ts` file a glyph outside a string would not parse, and comment lines are
   * dropped before this runs, so widening costs nothing and closes the hole.
   */
  it("prints no raw macOS glyph outside the shortcut tables", () => {
    const hits = scan(GLYPHS).filter((hit) => !TABLES.has(hit.file));
    expect(
      hits,
      "a raw macOS glyph reaches non-Mac readers; route it through " +
        "shortcutKeyLabel / shortcutTextLabel:\n" + describeHits(hits),
    ).toEqual([]);
  });

  /**
   * The defect that shipped: the palette rewrote ⌘ and ⇧ inline and therefore
   * missed ⌃ and doubled separators. Substitution belongs to one function.
   */
  it("rewrites glyphs in exactly one place", () => {
    const hits = scan(/\.replaceAll?\(\s*["'`][⌘⌥⌃⇧]/, false);
    expect(
      hits,
      "hand-rolled glyph substitution drifts from the real mapping:\n" + describeHits(hits),
    ).toEqual([]);
  });
});
