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

/** Whether a line begins inside a comment an earlier line opened. */
type CommentState = "code" | "block" | "html";

const COMMENT_CLOSERS = { block: "*/", html: "-->" } as const;

/**
 * The part of a line outside comments, where naming a platform is output and
 * not explanation, plus the state the NEXT line begins in.
 *
 * Carrying state is the whole point. The house style indents the continuation
 * lines of a block comment as prose rather than gutter-marking them with `*`:
 *
 *     /* Slash-alpha, not a flat token: an alpha-less fill is a slab that paints
 *        over the macOS window material instead of reading as a recess in it. *\/
 *
 * A per-line test sees line two start with a letter, calls it template text and
 * reports "macOS" as a stray — which it is not. 629 comment lines in the scanned
 * trees read that way when this was written; the guard was green only because
 * none of them named a platform, and the first that did got reworded to appease
 * a bug rather than fixing it.
 * So the opener is tracked until its closer, in `/* *\/`, `<!-- -->` and the
 * `{/* *\/}` form Svelte allows (the braces are ordinary characters either way).
 *
 * Everything after a closer on the same line stays visible: `<!-- why --> ⌘K`
 * really does print a glyph, and the old line test skipped that line whole.
 *
 * `//` is honoured only where the trimmed line starts with it, as before: a bare
 * `//` mid-line is a URL far more often than a comment, and in a template or a
 * `<style>` block it is never a comment at all. Checking it first also stops a
 * `/*` quoted inside a line comment from opening a block that was never opened.
 *
 * Deliberately not `portable-paths.contract`'s `code()`, which masks quoted text
 * before stripping: that is right for the `.ts` trees it reads and wrong here,
 * where an apostrophe in template prose ("the reader's view") is not a quote and
 * masking spans between two of them would hide markup from the scan. The cost is
 * that a comment opener inside a string — `"src/**\/*.ts"` — desynchronises the
 * tracker. So rather than trust it, "every scanned file closes what it opens" is
 * asserted below: a blinded scanner must not read as a clean one.
 */
function outsideComments(line: string, state: CommentState): { visible: string; next: CommentState } {
  if (state === "code" && line.trim().startsWith("//")) return { visible: "", next: "code" };
  let visible = "";
  let rest = line;
  let current = state;
  for (;;) {
    if (current === "code") {
      const block = rest.indexOf("/*");
      const html = rest.indexOf("<!--");
      if (block < 0 && html < 0) return { visible: visible + rest, next: "code" };
      const opensBlock = html < 0 || (block >= 0 && block < html);
      const at = opensBlock ? block : html;
      visible += rest.slice(0, at);
      rest = rest.slice(at + (opensBlock ? "/*".length : "<!--".length));
      current = opensBlock ? "block" : "html";
    } else {
      const closer = COMMENT_CLOSERS[current];
      const at = rest.indexOf(closer);
      if (at < 0) return { visible, next: current };
      rest = rest.slice(at + closer.length);
      current = "code";
    }
  }
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

/**
 * One file's hits, plus the comment state its last line leaves open.
 *
 * Takes the source rather than reading it, so the tests below can run real
 * multi-line markup through the same engine the trees are scanned with. A guard
 * whose own cases exercise a reimplementation proves nothing about the guard.
 */
function scanSource(
  file: string,
  source: string,
  pattern: RegExp,
  stripMapperCalls = true,
): { hits: Hit[]; unclosed: CommentState } {
  const hits: Hit[] = [];
  let inTemplate = !file.endsWith(".svelte");
  let state: CommentState = "code";
  source.split("\n").forEach((line, index) => {
    const outside = outsideComments(line, state);
    state = outside.next;
    if (outside.visible.includes("</script>")) {
      inTemplate = true;
      return;
    }
    const mapped = stripMapperCalls ? outside.visible.replace(MAPPER_CALL, "") : outside.visible;
    for (const candidate of visibleText(file, mapped, inTemplate)) {
      if (pattern.test(candidate)) {
        hits.push({ file, line: index + 1, text: candidate.trim().slice(0, 90) });
      }
    }
  });
  return { hits, unclosed: state };
}

function scan(pattern: RegExp, stripMapperCalls = true): Hit[] {
  return scannedFiles()
    .filter((file) => !OWNERS.has(file))
    .flatMap(
      (file) =>
        scanSource(file, readFileSync(join(ROOT, file), "utf8"), pattern, stripMapperCalls).hits,
    );
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

/**
 * A comment cannot reach a reader, so it is skipped for its whole length —
 * and the moment it ends, scanning resumes. Both halves matter: skipping too
 * little reports prose about the defect AS the defect (the bug these cases
 * pin), and skipping too much turns the guard off.
 *
 * Every case runs real markup through `scanSource`, the same engine the trees
 * are scanned with, and asserts the LINE NUMBERS reported.
 */
describe("reads comments as explanation and everything else as output", () => {
  /** The `<style>` shape that provoked this: prose wrapped without a gutter. */
  const STYLE_BLOCK = [
    `<script lang="ts">`,
    `  let { open = false } = $props();`,
    `</script>`,
    ``,
    `<div class="sheet" class:open></div>`,
    ``,
    `<style>`,
    `  .sheet {`,
    `    /* Slash-alpha, not a flat token: an alpha-less fill is a slab that paints`,
    `       over the macOS window material instead of reading as a recess in it. */`,
    `    background: color-mix(in oklab, var(--gp-surface) 82%, transparent);`,
    `  }`,
    `</style>`,
  ].join("\n");

  /**
   * One comment and one paragraph, each wrapping onto a second line, each
   * naming the platform. Only the paragraph prints.
   */
  const WRAPPED_MARKUP = [
    `<script lang="ts">`,
    `  let { granted = false } = $props();`,
    `</script>`,
    ``,
    `<!-- Permission is the host's to grant, so this copy names the host rather`,
    `     than macOS, which is only where the author happened to be. -->`,
    `<p class="gp-note">`,
    `  Desktop notifications stay silent until you allow them in`,
    `  macOS System Settings, which GitPulse cannot do for you.`,
    `</p>`,
  ].join("\n");

  /** The braced form, which Svelte allows and which wraps the same way. */
  const BRACED_COMMENT = [
    `<script lang="ts">`,
    `  let { chord } = $props();`,
    `</script>`,
    ``,
    `{/* The catalog authors this chord in macOS notation and CoachMark maps`,
    `    it for every caller, so nothing below reaches a reader raw. */}`,
    `<span class="gp-keycap">{chord}</span>`,
    `<span class="gp-keycap">⌘K</span>`,
  ].join("\n");

  /** In `.ts` only literals are read — including literals inside comments. */
  const TS_MODULE = [
    `/* The packager produces the bundle, not this module, so the error says`,
    `   "the application bundle" and never "the macOS bundle": the host that`,
    `   hit it may not be a Mac. */`,
    `export const MISSING = "Install the application bundle first.";`,
    `export const WRONG = "Install the macOS application bundle first.";`,
  ].join("\n");

  const lines = (file: string, source: string, pattern: RegExp): number[] =>
    scanSource(file, source, pattern).hits.map((hit) => hit.line);

  it("skips a continuation line of a block comment in a style block", () => {
    expect(lines("src/lib/components/Sheet.svelte", STYLE_BLOCK, PLATFORM_WORDS)).toEqual([]);
  });

  it("still reports a continuation line of real markup", () => {
    // Line 6 is the comment's second line, line 9 the paragraph's. Only 9.
    expect(lines("src/lib/components/Notice.svelte", WRAPPED_MARKUP, PLATFORM_WORDS)).toEqual([9]);
  });

  it("skips the braced comment form and keeps scanning after it", () => {
    const file = "src/lib/components/Chord.svelte";
    expect(lines(file, BRACED_COMMENT, PLATFORM_WORDS)).toEqual([]);
    expect(lines(file, BRACED_COMMENT, GLYPHS)).toEqual([8]);
  });

  it("scans text that follows a comment closing on the same line", () => {
    // Line 5 STARTS with `<!--`, so the old line test skipped it whole — glyph
    // and all. A closed comment ends at its closer, not at the line's end.
    const source = [
      `<script lang="ts">`,
      `  let { chord } = $props();`,
      `</script>`,
      ``,
      `<!-- authored in Mac notation, mapped on the way out --> <kbd>⌘K</kbd>`,
    ].join("\n");
    expect(lines("src/lib/components/Keycap.svelte", source, GLYPHS)).toEqual([5]);
  });

  it("skips a quoted phrase inside a comment but not the literal below it", () => {
    expect(lines("src/lib/desktop/bundleCopy.ts", TS_MODULE, PLATFORM_WORDS)).toEqual([5]);
  });

  /**
   * The one way this tracking can fail silently: an opener it should not have
   * believed — `"src/**\/*.ts"` in a literal — leaves a comment open to the end
   * of the file, and every line after it is skipped as prose. A file that never
   * closes what it opens would not compile, so the state is the scanner's own
   * report on whether it read the whole file or stopped early.
   */
  it("closes every comment it opens, so no file is skipped wholesale", () => {
    const files = scannedFiles();
    expect(files.length, "the walk found no files to scan").toBeGreaterThan(50);
    const unclosed = files.filter(
      (file) =>
        scanSource(file, readFileSync(join(ROOT, file), "utf8"), PLATFORM_WORDS).unclosed !==
        "code",
    );
    expect(
      unclosed,
      "the scanner stopped reading these files partway, so their remaining " +
        "lines were skipped rather than checked:\n" + unclosed.join("\n"),
    ).toEqual([]);
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
