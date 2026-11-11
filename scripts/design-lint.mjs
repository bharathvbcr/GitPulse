#!/usr/bin/env node
/**
 * design-lint: the mechanical half of docs/design/UI_RULES.md section 1.
 *
 * It implements rules 1-18. Rule 14 (banned copy terms) reads the list owned
 * by the vocabulary (`BANNED_UI_TERMS` in src/lib/ui/copy/vocabulary.ts); it is
 * one entry in the flat `RULES` registry below. That list is a .ts file, which
 * node loads by type stripping (node 22.18+ / 24+; the repo runs node 26).
 *
 * RATCHET. `--baseline` records per-rule, per-file violation counts in
 * scripts/design-lint.baseline.json together with the commit they were taken
 * at. A normal run exits 1 only when a (rule, file) count rises above the
 * baseline, or a file that was not in the baseline gains a violation.
 * Decreases are printed as progress. Exit codes: 0 holds, 1 ratchet broken,
 * 2 the check could not run (a check that could not run never looks like a
 * check that passed).
 *
 * SCOPE. `src/**` `.svelte`, `.ts` and `.css`, minus `*.test.ts`, `*.spec.ts`,
 * `__tests__/`, `*.d.ts` and files whose first lines say they are generated.
 * Only `node_modules`, `.git` and `.svelte-kit` are skipped by directory name, at any depth:
 * a source directory called `coverage` (src/lib/coverage) used to be skipped by name, which
 * is how `coverage/format.ts` shipped `bg-emerald-500/15` past rule 3 in two audits.
 * Each kind of file is read by the rules that apply to it:
 *   .svelte  markup, the scripts (rules 3, 9, 10, 13) and the `<style>` blocks: their
 *            declarations (rules 1, 2, 4, 6, 8, 9, 10, 11, 12) and their `@apply`
 *            utilities, which are class tokens like any other;
 *   .ts      string literals only (never identifiers, imports or comments), by rules 1,
 *            3, 9, 10 and 14 (see rule 14 for what counts as copy in a `.ts` file);
 *   .css     src/app.css and the other stylesheets, by rules 1, 3, 4, 8, 9, 10, 17 and
 *            18, plus rule 15 on src/app.css alone. Rule 7 never reads a stylesheet: it
 *            is where the allow-listed utilities (`@utility gp-menu-sep`, ...) are defined.
 *   A stylesheet OWNS its tokens. A custom property declaration (`--c-bg: rgb(...)`,
 *   `--gp-dur-fast: 140ms`, `--gp-ease-out: cubic-bezier(...)`, everything in `@theme` and
 *   `:root`) is where the one permitted literal of that token lives, and a mask
 *   declaration's `#000` is an alpha channel, not a colour: neither is read by any rule
 *   (isOwnedDeclaration). A rule that USES a token reads it through `var(--x)` and is
 *   checked like any other. Nothing else in a stylesheet is exempt: not a primitive
 *   (`gp-btn`, `gp-field`), not a keyframe recipe; they are findings, held by the
 *   ratchet. The one named exception is `gp-slide-docked`, rule 9's docked-sidebar width
 *   transition (DESIGN_LANGUAGE section 8.5).
 *
 * HOW IT READS A FILE (this is what keeps false positives out)
 *   - A real tokenizer, not a line grep. A `.svelte` file is split into tags
 *     (with attributes and `{...}` mustaches, string- and brace-aware), text
 *     nodes, `<script>` and `<style>` bodies. JavaScript is lexed (strings,
 *     template literals, regex literals, comments) so a `...` spread, a URL
 *     and a comment are never mistaken for UI text.
 *   - Comments (HTML, JS, CSS) are blanked before any matcher runs, so a hex
 *     color or a banned class named in a comment is never a hit.
 *   - Tailwind classes are matched as tokens, after the variants
 *     (`md:hover:`), the `!` and the `/opacity` are peeled off, and only
 *     inside `class="..."`, `class:name`, `class={...}` string literals,
 *     `*Class`-named props, and string literals in scripts.
 *
 * RULES (matcher, then what it deliberately does not see)
 *   1  type scale: token `text-[Npx]` (also in a `@apply` and in a `.ts` string) and a
 *      `font-size: Npx` declaration in `<style>`, `style=` or a stylesheet. Not
 *      `text-[var(--x)]`, `text-[length:...]`, rem/em sizes (only the px form is a literal pixel
 *      size) or the `font` shorthand.
 *   2  weight and case: tokens `font-thin|extralight|light|bold|extrabold|black`;
 *      `font-weight:` other than 400/500/600/normal/inherit/var(); a class
 *      source holding `uppercase` and `tracking-*` together; a CSS rule with
 *      `text-transform: uppercase` and `letter-spacing`. Misses `font-weight`
 *      set from a script expression.
 *   3  palette: `{text,bg,border[-side],ring,from,via,to,fill,stroke,divide,...}-
 *      <palette>-<shade>` tokens, and any `dark:` token that is a color
 *      utility (text-/bg-/border-/ring-... but not sizes, alignment, widths or
 *      arbitrary values). Applies to `.ts` strings and to `@apply` too. Misses `text-white`,
 *      `bg-black` (not palette names) and colors built by string concatenation.
 *   4  literals: `#hex` and `rgb(12 34 56)`/`hsl(...)` literals in `.svelte`
 *      markup attributes, mustaches and `<style>` declaration values, and in the
 *      declaration values of a stylesheet (token definitions and masks excepted, see
 *      SCOPE; a box-shadow or gradient recipe written with a literal is a hit). In
 *      `<style>` and `style=` any position counts; elsewhere the hex must be a
 *      whole quoted value or sit in a Tailwind `[#fff]` (so "Issue #123" and
 *      `querySelector("#add")` are not hits). `<script>` is exempt on purpose:
 *      the xterm and canvas adapters convert tokens at their boundary.
 *      `href`/`id`/`for`/`src`-style attributes and `url(#id)` are skipped.
 *   5  radius: tokens `rounded[-side]-[...]` unless the value is
 *      `var(--gp-r-*)`, and `rounded[-side]-3xl`. Misses `border-radius: Npx`
 *      in `<style>` ("picked by eye" has no mechanical test there).
 *   6  z-index: tokens `z-N` (N>0) and `z-[...]`; `z-index: N` (N != 0) in
 *      `<style>`, `style=` and `style:z-index`. `z-0` and `var()` pass.
 *   7  hairlines: tokens `border-{t,b,l,r,x,y,s,e}` and `border-X-N` (N>0),
 *      `divide-x|y[-N]`. Exempt: a class source on an element that also wears
 *      an allow-listed utility (gp-menu-sep, gp-separator, gp-card, gp-field,
 *      gp-field-multi, gp-btn, gp-chip, gp-floor, gp-select). `gp-section-edge`
 *      is NOT an exemption: it is the shim whose call sites are being removed.
 *      Misses `border-top:` written in `<style>`.
 *   8  border opacity: tokens `border*-<color>/N` and `divide-<color>/N` whose
 *      N is not 35, 65, 90 (or `[0.35]`, `[0.65]`, `[0.9]`); and `<style>`
 *      border/outline declarations using `/ 0.N` outside those stops.
 *   9  layout transitions: tokens `transition-all`, `transition-shadow`,
 *      `transition-[...]` and `[transition:...]`/`[transition-property:...]` naming
 *      width/height/top/left/margin/padding/filter/box-shadow/backdrop-filter/all/...;
 *      any arbitrary animation (`animate-[...]`, `[animation:...]` other than none),
 *      which escapes the reduced-motion lists rule 15 checks; the same properties
 *      in `<style>` `transition`/`transition-property`. Class strings in `.ts` files
 *      are read too. A bare `transition` token is not
 *      flagged (Tailwind's default list includes box-shadow and filter, but the
 *      rule text names only the explicit forms). The `gp-slide-docked` utility in
 *      app.css, the docked sidebar's one-shot width slide, is the single named
 *      exception (LAYOUT_TRANSITION_OWNERS); the same properties in a stylesheet
 *      rule or a stylesheet `@apply` are otherwise hits.
 *   10 motion literals: tokens `duration-N|[..]`, `ease-in|out|in-out|linear|[..]`;
 *      a time or easing literal inside an `animate-[...]`/`transition-[...]`
 *      arbitrary value (`animate-[x_200ms_ease-out]`);
 *      `duration: N` and `motionDuration(N` inside script and mustache code
 *      (so UI text such as "Duration: 5" is safe); `cubic-bezier(`; time
 *      literals in `<style>` and stylesheet `transition`/`animation`/`*-duration`
 *      declarations (a duration inside a `--gp-dur-*` definition is not one). The
 *      owners (`src/lib/motion/**`, `src/lib/ui/transitions.ts`) are exempt.
 *      Applies to `.ts` for the code forms only. Misses `setTimeout(fn, 180)`,
 *      `transition-delay` and `animation-delay`.
 *   11 blur: tokens `backdrop-blur[-x]`, `backdrop-filter|brightness|...`;
 *      `backdrop-filter` declarations other than `none` in `<style>`/`style=`.
 *   12 widget dialects: a `<style>` selector class named like a button, link,
 *      chip, pill, toggle or tab (`.link`, `.btn`, `.chip-remove`, `.icon-button`,
 *      `.text-action`, `.close-alert`, `.nav-link`). Names inside `:not()` and
 *      `@keyframes` are skipped. Misses widgets named by an unrelated word.
 *   13 ellipsis: `...` (exactly three dots) in markup text, in the string
 *      attributes that carry display text (title, placeholder, aria-label,
 *      alt, label, ... and any prop of a component), and in JS string/template
 *      literals of a `.svelte` file. Spread (`...args`) is code, not a string,
 *      so it is never seen; strings passed to `console.*` are skipped. Misses
 *      user text assembled in a `.ts` file (the rule is `.svelte` only).
 *   14 banned UI terms and retired verbs. Terms: every `BANNED_UI_TERMS` pattern over
 *      the text a person sees (below), in `.svelte` and `.ts`. Retired verbs:
 *      every `RETIRED_VERB_TERMS` entry (Reload, Recheck, Retry, Launch, Organise),
 *      but only at the START of a LABEL, never in prose. A label is the text of a
 *      button, link, summary or option, of anything with an action role
 *      (button, menuitem, tab, ...), of a component named like a button or menu item,
 *      of anything wearing gp-btn, gp-icon-btn, gp-menu-item, gp-chip or gp-seg-btn,
 *      a `label=`, `aria-label=`, `title=`, `data-tip=`, `tooltip=` or `text=`
 *      attribute on those or on any component, and in a `.ts` file the value of a
 *      `label`, `title`, `ariaLabel`, `actionLabel`, `buttonLabel`, `cta`, `tooltip` or
 *      `paletteCommand` key. "Retry search" and "Reload" are hits; a paragraph that
 *      says "Retry later" and a heading are not. Not seen: a label assembled in a
 *      variable (`{label}`), a verb that is not the first word, and `Retry` as a bare
 *      `return "Retry"` in a `.ts` file. The retired-verb hits carry the verb as
 *      their term, so an allow entry can excuse one word (BrowserSession's browser
 *      "Reload" is the browser's own). The rest of this entry is the banned terms.
 *      Text a person sees, in `.svelte`: markup text nodes, the display
 *      attributes of rule 13 (title, aria-label, placeholder, alt, label, ...,
 *      on elements and component props alike), and the string literals inside
 *      a mustache in a text position or in such an attribute
 *      (`{busy ? "Retry attempt" : "Done"}`, `title={x ? "Open repo" : "Close"}`).
 *      Within a mustache a string is CODE, not copy, and skipped, when it is an
 *      operand of ==, ===, != or !== (`kind === "attempt"`), an argument of a
 *      call (`invoke("devmap_status")`, `mapFailureMessage("no_cli")`), or sits
 *      in a block tag (`{#if}`, `{:else if}`, `{@const}`) or an event-handler
 *      prop (`onTools={() => ...}`). NOT seen: `<script>` strings (imports,
 *      invoke names, paths; rule 13 does read those, rule 14 does not),
 *      comments, `class`/`style`, and `data-*` attributes (`data-testid`;
 *      `data-tip`/`data-tooltip` are display and are read).
 *      In a `.ts` file there is no markup, so the string literal itself and its
 *      neighbours decide (isTsCopy): a literal of two or more words, or a single word
 *      that is the value of a display key (label, title, description, heading, ...),
 *      is copy. Not copy: an identifier-shaped literal (no whitespace: a kind, id,
 *      key, path or event name), an object key, an operand of ==/===/case, an import
 *      specifier, the argument of invoke/querySelector/getItem/classList/RegExp/
 *      includes and similar, a console argument, a value under a keywords, id, kind,
 *      type or path key, the vocabulary file and *.fixtures.ts. A snake_case word is
 *      not a hit in an `Error(...)` message (an assertion about a wire field), after
 *      a `.`, `/` or backtick, before `=`, `(` or `/`, or in a line of source. Blind
 *      spots, on top of the ones below: prose assembled from single words, agent
 *      prompts and developer diagnostics (both are copy in this reading; read the hit
 *      and allow-list the file with a reason). Blind spots: copy
 *      passed to a call (`plural(n, "file")`) is skipped with the other
 *      arguments; copy assembled in script is not seen; component props that
 *      carry a non-copy value as plain text (`only="devmap"`,
 *      `field="max_live_runs"`) count, as they do for rule 13. The patterns are
 *      text approximations (see each entry note in the vocabulary), so
 *      "project" in a noun phrase the glossary allows and the snake_case shape
 *      in prose are candidates, not proof. When two entries match the same
 *      words ("repo map" and "repo") each reports once.
 *   15 reduced motion: in src/app.css, every class that animates (an
 *      `animation`/`transition` declaration other than `none`, or an `@apply`
 *      of `transition*`/`animate-*`) must be named in both the
 *      `@media (prefers-reduced-motion: reduce)` list and the
 *      `html[data-motion="reduced"]` list, and the two lists must name the
 *      same classes. Rules already gated by `no-preference` plus
 *      `:not([data-motion="reduced"])` are safe by construction. Blind spots:
 *      the reduced-TRANSPARENCY half of the rule (a translucent surface missing
 *      from that block) is not mechanically detectable and stays with the paired
 *      contract tests; animated rules with no class in their subject (the range
 *      thumb) and animations in scoped `<style>` blocks are not examined.
 *   16 heading separator: a gp-separator directly under a section heading.
 *   17 undefined gp-* class: a gp-* token in a `class`, `*Class` or `class:name` attribute of
 *      a component, or in an `@apply`, that no stylesheet defines (no `@utility gp-x` and
 *      no `.gp-x` in any selector of a stylesheet or a component's `<style>`, `:not()`
 *      excluded). It styles nothing, whatever the markup looks like. Reported at the use,
 *      so the ratchet is per file. NOT read: `.ts` and script strings, where the same
 *      literal is a storage key, an id or an event name, or the class of an SVG string
 *      with its own embedded stylesheet (gp-boot, gp-task, src/lib/pulse/exportCard.ts
 *      all hold a gp-* word that is not a class this rule should define). A class built
 *      by interpolation (`gp-seg-{tone}`) is a prefix and is not checked. A class that is
 *      only a hook for a test or a script is a hit too: give it a data-* hook or style it.
 *   18 unused gp-* class: a class a stylesheet defines (the same definitions) that no
 *      markup, script, `.ts` string or `@apply` mentions, outside `<style>` bodies, comments,
 *      tests, and index.html and public/*.js (read as extra text). Reported at the
 *      first definition. Generous on purpose: any whole-word mention keeps a class
 *      alive, and a prefix ending in a hyphen (`gp-seg-{tone}`) keeps every class that
 *      starts with it. Blind spot: a class built from pieces sharing no literal prefix
 *      is reported as unused when it is not. Rules 17 and 18 need the whole tree, so
 *      they run in lintFiles/lintRepo (RULES entries with runRepo), not lintSource.
 *
 * GATE. `--strict` (the `design:gate` script, a step of `bun run ci:local`) adds
 * what the ratchet alone does not say: every violation must be either fixed or
 * named in scripts/design-lint.config.json. `allow` entries (permanent, each with a
 * reason: brand art with no token) are printed as "allowed" and never dropped;
 * `deferred` entries (a file another lane is rewriting) are held to the ratchet
 * only. An entry that matches no violation fails the gate, so an exemption
 * cannot outlive its cause. To re-baseline to zero: delete the deferred entries
 * once their files are clean and retake the baseline.
 *
 * Flags: --baseline (write the baseline; refuses a dirty src/ unless
 * --allow-dirty), --strict, --config-file FILE, --json, --list [--rule N] (print every violation),
 * --root DIR, --baseline-file FILE, --help.
 */
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BANNED_UI_TERMS, RETIRED_VERB_TERMS } from "../src/lib/ui/copy/vocabulary.ts";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_BASELINE_FILE = path.join(REPO_ROOT, "scripts", "design-lint.baseline.json");
export const APP_CSS = "src/app.css";

/**
 * @typedef {{ start: number, end: number, text: string }} Lit
 * @typedef {{ raw: string, util: string, variants: string[], opacity: string | null, start: number }} Token
 * @typedef {{ tokens: Token[], views: Token[][], tag: number, exempt: boolean }} ClassSource
 * @typedef {{ selector: string, selStart: number, at: boolean, parent: CssRule | null, decls: Decl[], applies: string[], applyList: Lit[], applyStart: number }} CssRule
 * @typedef {{ prop: string, value: string, start: number, end: number, rule: CssRule }} Decl
 * @typedef {{ type: "text" | "expr", start: number, end: number }} Seg
 * @typedef {{ tag: number, tagName: string, name: string, nameStart: number, segs: Seg[] }} Attr
 * @typedef {{ text: string, start: number, css: boolean }} ColorRegion
 * @typedef {{ file: string, kind: "svelte" | "ts" | "css", src: string, clean: string, classSources: ClassSource[], decls: Decl[], cssRules: CssRule[], userStrings: Lit[], copyStrings: Lit[], labels: Lit[], colorRegions: ColorRegion[], codeRegions: [number, number][], styleRanges: [number, number][] }} Model
 * @typedef {{ rule: number, file: string, line: number, col: number, message: string, term?: string }} Violation
 * @typedef {(offset: number, message: string, term?: string) => void} Report
 * @typedef {(file: string, offset: number, message: string, term?: string) => void} RepoReport
 * @typedef {{ id: number, name: string, summary: string, svelte: boolean, ts: boolean, css: boolean, run?: (m: Model, report: Report) => void, runRepo?: (models: Model[], report: RepoReport, extraText: string) => void }} Rule
 */

// ---------------------------------------------------------------------------
// JavaScript lexer: strings, template literals, regex literals, comments.
// ---------------------------------------------------------------------------

const REGEX_PRECEDERS = new Set(["return", "typeof", "case", "in", "of", "delete", "void", "throw", "new", "else", "do", "instanceof", "yield", "await"]);

/** @typedef {{ strings: Lit[], comments: [number, number][] }} JsOut */

/** @returns {JsOut} */
function newJsOut() {
  return { strings: [], comments: [] };
}

/**
 * Lex `src[from, to)` and collect string literals and comment ranges.
 * With `stopAtBrace` it returns the index of the unmatched `}` (so a
 * mustache or `${}` can be skipped as a unit), else `to`.
 * @param {string} src @param {number} from @param {number} to
 * @param {JsOut} out @param {boolean} stopAtBrace
 * @returns {number}
 */
function lexJs(src, from, to, out, stopAtBrace) {
  let i = from;
  let depth = 0;
  let prev = "";
  let word = "";
  while (i < to) {
    const c = src[i];
    const n = src[i + 1];
    if (c === "/" && n === "/") {
      let j = src.indexOf("\n", i);
      if (j < 0 || j > to) j = to;
      out.comments.push([i, j]);
      i = j;
      continue;
    }
    if (c === "/" && n === "*") {
      let j = src.indexOf("*/", i + 2);
      j = j < 0 || j + 2 > to ? to : j + 2;
      out.comments.push([i, j]);
      i = j;
      continue;
    }
    if (c === '"' || c === "'") {
      let j = i + 1;
      while (j < to && src[j] !== c && src[j] !== "\n") {
        if (src[j] === "\\") j++;
        j++;
      }
      const end = Math.min(j, to);
      out.strings.push({ start: i + 1, end, text: src.slice(i + 1, end) });
      i = j + 1;
      prev = '"';
      word = "";
      continue;
    }
    if (c === "`") {
      i = lexTemplate(src, i, to, out);
      prev = '"';
      word = "";
      continue;
    }
    if (c === "/") {
      const regexAllowed = prev === "" || "(,=:[!&|?{};+-*%<>~^".includes(prev) || (prev === "w" && REGEX_PRECEDERS.has(word));
      if (regexAllowed) {
        let j = i + 1;
        let inClass = false;
        let closed = false;
        for (; j < to; j++) {
          const d = src[j];
          if (d === "\n") break;
          if (d === "\\") {
            j++;
            continue;
          }
          if (d === "[") inClass = true;
          else if (d === "]") inClass = false;
          else if (d === "/" && !inClass) {
            closed = true;
            break;
          }
        }
        if (closed) {
          i = j + 1;
          while (i < to && /[a-z]/.test(src[i])) i++;
          prev = "w";
          word = "";
          continue;
        }
      }
      prev = "/";
      word = "";
      i++;
      continue;
    }
    if (c === "{") depth++;
    else if (c === "}") {
      if (depth === 0 && stopAtBrace) return i;
      if (depth > 0) depth--;
    }
    if (/[\w$]/.test(c)) {
      if (i === from || !/[\w$]/.test(src[i - 1])) word = "";
      word += c;
      prev = "w";
    } else if (!/\s/.test(c)) {
      prev = c;
      word = "";
    }
    i++;
  }
  return to;
}

/**
 * @param {string} src @param {number} i index of the opening backtick
 * @param {number} to @param {JsOut} out
 * @returns {number} index after the closing backtick
 */
function lexTemplate(src, i, to, out) {
  let segStart = i + 1;
  let j = i + 1;
  while (j < to) {
    const c = src[j];
    if (c === "\\") {
      j += 2;
      continue;
    }
    if (c === "`") {
      out.strings.push({ start: segStart, end: j, text: src.slice(segStart, j) });
      return j + 1;
    }
    if (c === "$" && src[j + 1] === "{") {
      out.strings.push({ start: segStart, end: j, text: src.slice(segStart, j) });
      j = lexJs(src, j + 2, to, out, true);
      segStart = j + 1;
      j++;
      continue;
    }
    j++;
  }
  out.strings.push({ start: segStart, end: to, text: src.slice(segStart, to) });
  return to;
}

// ---------------------------------------------------------------------------
// CSS reader (comment-free input): rules, declarations, @apply.
// ---------------------------------------------------------------------------

/** @param {string} selector @param {number} selStart @param {CssRule | null} parent @returns {CssRule} */
function newCssRule(selector, selStart, parent) {
  return { selector, selStart, at: selector.startsWith("@"), parent, decls: [], applies: [], applyList: [], applyStart: selStart };
}

/**
 * @param {string} text comment-free CSS (a `<style>` body, a style attribute or a stylesheet)
 * @param {number} base absolute offset of text[0]
 * @returns {{ root: CssRule, rules: CssRule[] }}
 */
function parseCss(text, base) {
  const root = newCssRule("", base, null);
  /** @type {CssRule[]} */
  const rules = [];
  /** @type {CssRule[]} */
  const stack = [root];
  let seg = 0;
  /** @param {string} s @param {number} off */
  const flush = (s, off) => {
    const top = stack[stack.length - 1];
    const trimmed = s.trim();
    if (trimmed === "") return;
    if (trimmed.startsWith("@apply")) {
      const value = trimmed.slice(6).trim();
      const at = base + off + (s.length - s.trimStart().length) + (trimmed.length - trimmed.slice(6).trimStart().length);
      top.applies.push(value);
      top.applyList.push({ start: at, end: at + value.length, text: value });
      top.applyStart = base + off;
      return;
    }
    const m = /^([a-zA-Z-]+|--[\w-]+)\s*:\s*([\s\S]*)$/.exec(trimmed);
    if (!m) return;
    const start = base + off + (s.length - s.trimStart().length);
    top.decls.push({ prop: m[1].toLowerCase(), value: m[2].trim(), start, end: start + trimmed.length, rule: top });
  };
  let quote = "";
  let paren = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === "\\") i++;
      else if (ch === quote) quote = "";
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === "(") paren++;
    else if (ch === ")") paren = Math.max(0, paren - 1);
    if (paren > 0) continue;
    if (ch === "{") {
      const prelude = text.slice(seg, i);
      const lead = prelude.length - prelude.trimStart().length;
      const rule = newCssRule(prelude.trim(), base + seg + lead, stack[stack.length - 1]);
      rules.push(rule);
      stack.push(rule);
      seg = i + 1;
    } else if (ch === ";") {
      flush(text.slice(seg, i), seg);
      seg = i + 1;
    } else if (ch === "}") {
      flush(text.slice(seg, i), seg);
      if (stack.length > 1) stack.pop();
      seg = i + 1;
    }
  }
  flush(text.slice(seg), seg);
  return { root, rules };
}

/** @param {string} text @returns {[number, number][]} */
function cssCommentRanges(text) {
  /** @type {[number, number][]} */
  const out = [];
  const re = /\/\*[\s\S]*?(?:\*\/|$)/g;
  let m;
  while ((m = re.exec(text)) !== null) out.push([m.index, m.index + m[0].length]);
  return out;
}

/**
 * Replace the ranges with spaces, keeping newlines so offsets and lines hold.
 * @param {string} src @param {[number, number][]} ranges
 */
function blank(src, ranges) {
  if (ranges.length === 0) return src;
  const sorted = [...ranges].sort((a, b) => a[0] - b[0]);
  let out = "";
  let pos = 0;
  for (const [s, e] of sorted) {
    if (e <= pos) continue;
    const from = Math.max(s, pos);
    out += src.slice(pos, from) + src.slice(from, e).replace(/[^\n]/g, " ");
    pos = e;
  }
  return out + src.slice(pos);
}

// ---------------------------------------------------------------------------
// Svelte markup parser.
// ---------------------------------------------------------------------------

/**
 * Index of the `}` closing the mustache that opens at `src[open]`. A block
 * close tag (`{/if}`) holds no expression, and lexing it would read its `/`
 * as the start of a regex literal.
 * @param {string} src @param {number} open @param {number} n @param {JsOut} out
 */
function mustacheEnd(src, open, n, out) {
  if (src[open + 1] === "/") {
    const close = src.indexOf("}", open);
    return close < 0 ? n : close;
  }
  return lexJs(src, open + 1, n, out, true);
}

/** Elements with no closing tag: they never become a parent. */
const VOID_ELEMENTS = new Set(["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"]);

/**
 * @param {string} src
 */
function parseSvelte(src) {
  const n = src.length;
  /** @type {{ start: number, end: number }[]} */ const scripts = [];
  /** @type {{ start: number, end: number }[]} */ const styles = [];
  /** @type {Attr[]} */ const attrs = [];
  /** @type {{ start: number, end: number, parent: number }[]} */ const texts = [];
  /** @type {{ start: number, end: number, parent: number }[]} */ const exprs = [];
  /** @type {{ tag: number, name: string }[]} */ const open = [];
  const parentTag = () => (open.length > 0 ? open[open.length - 1].tag : -1);
  /** @type {[number, number][]} */ const htmlComments = [];
  /** @type {string[]} */ const tags = [];
  const markupJs = newJsOut();
  let textStart = -1;
  /** @param {number} end */
  const flush = (end) => {
    if (textStart >= 0 && end > textStart) texts.push({ start: textStart, end, parent: parentTag() });
    textStart = -1;
  };
  /** @param {number} start @returns {number} */
  const parseTag = (start) => {
    const nm = /^[A-Za-z][\w:.-]*/.exec(src.slice(start + 1, start + 120));
    const name = nm ? nm[0] : "";
    const tag = tags.push(name) - 1;
    let j = start + 1 + name.length;
    let selfClosed = false;
    while (j < n) {
      while (j < n && /\s/.test(src[j])) j++;
      if (j >= n) return n;
      if (src[j] === ">") {
        j++;
        break;
      }
      if (src[j] === "/" && src[j + 1] === ">") {
        j += 2;
        selfClosed = true;
        break;
      }
      if (src[j] === "{") {
        const e = mustacheEnd(src, j, n, markupJs);
        attrs.push({ tag, tagName: name, name: "{}", nameStart: j, segs: [{ type: "expr", start: j + 1, end: e }] });
        j = e + 1;
        continue;
      }
      const ns = j;
      while (j < n && !/[\s=>]/.test(src[j]) && !(src[j] === "/" && src[j + 1] === ">")) j++;
      const aname = src.slice(ns, j);
      if (aname === "") {
        j++;
        continue;
      }
      let k = j;
      while (k < n && /\s/.test(src[k])) k++;
      if (src[k] !== "=") {
        attrs.push({ tag, tagName: name, name: aname, nameStart: ns, segs: [] });
        continue;
      }
      j = k + 1;
      while (j < n && /\s/.test(src[j])) j++;
      /** @type {Seg[]} */
      const segs = [];
      const q = src[j];
      if (q === '"' || q === "'") {
        j++;
        let ts = j;
        while (j < n && src[j] !== q) {
          if (src[j] === "{") {
            if (j > ts) segs.push({ type: "text", start: ts, end: j });
            const e = mustacheEnd(src, j, n, markupJs);
            segs.push({ type: "expr", start: j + 1, end: e });
            j = e + 1;
            ts = j;
          } else j++;
        }
        if (j > ts) segs.push({ type: "text", start: ts, end: j });
        j++;
      } else if (q === "{") {
        const e = mustacheEnd(src, j, n, markupJs);
        segs.push({ type: "expr", start: j + 1, end: e });
        j = e + 1;
      } else {
        const ts = j;
        while (j < n && !/[\s>]/.test(src[j])) j++;
        segs.push({ type: "text", start: ts, end: j });
      }
      attrs.push({ tag, tagName: name, name: aname, nameStart: ns, segs });
    }
    if ((name === "script" || name === "style") && !selfClosed) {
      const close = new RegExp(`</${name}\\s*>`, "i");
      const rest = src.slice(j);
      const m = close.exec(rest);
      const end = m ? j + m.index : n;
      (name === "script" ? scripts : styles).push({ start: j, end });
      return m ? end + m[0].length : n;
    }
    if (!selfClosed && !VOID_ELEMENTS.has(name.toLowerCase())) open.push({ tag, name });
    return j;
  };
  let i = 0;
  while (i < n) {
    const c = src[i];
    if (c === "<" && src.startsWith("<!--", i)) {
      flush(i);
      const j = src.indexOf("-->", i + 4);
      const e = j < 0 ? n : j + 3;
      htmlComments.push([i, e]);
      i = e;
      continue;
    }
    if (c === "<" && /[A-Za-z]/.test(src[i + 1] ?? "")) {
      flush(i);
      i = parseTag(i);
      continue;
    }
    if (c === "<" && src[i + 1] === "/") {
      flush(i);
      const j = src.indexOf(">", i);
      const closing = /^[A-Za-z][\w:.-]*/.exec(src.slice(i + 2, i + 122));
      if (closing) {
        for (let k = open.length - 1; k >= 0; k--) {
          if (open[k].name === closing[0]) {
            open.length = k;
            break;
          }
        }
      }
      i = j < 0 ? n : j + 1;
      continue;
    }
    if (c === "{") {
      flush(i);
      const e = mustacheEnd(src, i, n, markupJs);
      exprs.push({ start: i + 1, end: e, parent: parentTag() });
      i = e + 1;
      continue;
    }
    if (textStart < 0) textStart = i;
    i++;
  }
  flush(n);
  return { scripts, styles, attrs, texts, exprs, htmlComments, tags, markupJs };
}

// ---------------------------------------------------------------------------
// Tailwind class tokens.
// ---------------------------------------------------------------------------

/**
 * Split a class string into tokens. Bracketed arbitrary values stay whole.
 * @param {string} text @param {number} base @returns {Token[]}
 */
function tokenize(text, base) {
  /** @type {Token[]} */
  const out = [];
  const n = text.length;
  let i = 0;
  while (i < n) {
    while (i < n && /[\s"'`{},;$\\()?|&=<>]/.test(text[i])) i++;
    if (i >= n) break;
    const s = i;
    let depth = 0;
    while (i < n) {
      const ch = text[i];
      if (ch === "[") depth++;
      else if (ch === "]") depth = Math.max(0, depth - 1);
      if (depth === 0 && /[\s"'`{},;$\\()?|&=<>]/.test(ch)) break;
      if (depth > 0 && /\s/.test(ch)) break;
      i++;
    }
    const tok = parseToken(text.slice(s, i), base + s);
    if (tok) out.push(tok);
  }
  return out;
}

/** @param {string} raw @param {number} start @returns {Token | null} */
function parseToken(raw, start) {
  if (raw === "") return null;
  /** @type {string[]} */
  const parts = [];
  let depth = 0;
  let last = 0;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (ch === "[" || ch === "(") depth++;
    else if (ch === "]" || ch === ")") depth = Math.max(0, depth - 1);
    else if (ch === ":" && depth === 0) {
      parts.push(raw.slice(last, i));
      last = i + 1;
    }
  }
  let util = raw.slice(last);
  const variants = parts;
  if (util.startsWith("!")) util = util.slice(1);
  if (util.endsWith("!")) util = util.slice(0, -1);
  if (/^-[a-zA-Z]/.test(util)) util = util.slice(1);
  let opacity = null;
  depth = 0;
  let slash = -1;
  for (let i = 0; i < util.length; i++) {
    const ch = util[i];
    if (ch === "[" || ch === "(") depth++;
    else if (ch === "]" || ch === ")") depth = Math.max(0, depth - 1);
    else if (ch === "/" && depth === 0) slash = i;
  }
  if (slash > 0) {
    const after = util.slice(slash + 1);
    if (/^\d+(?:\.\d+)?$/.test(after) || /^\[[^\]]*\]$/.test(after)) {
      opacity = after;
      util = util.slice(0, slash);
    }
  }
  return { raw, util, variants, opacity, start };
}

const CLASS_ATTR = /^(?:class|[A-Za-z]*[Cc]lass(?:es|Name)?)$/;
const ALLOW_LISTED = new Set(["gp-menu-sep", "gp-separator", "gp-card", "gp-field", "gp-field-multi", "gp-btn", "gp-chip", "gp-floor", "gp-select"]);

// ---------------------------------------------------------------------------
// Model builder.
// ---------------------------------------------------------------------------

/** @param {string} file */
function kindOf(file) {
  if (file.endsWith(".svelte")) return /** @type {const} */ ("svelte");
  if (file.endsWith(".css")) return /** @type {const} */ ("css");
  return /** @type {const} */ ("ts");
}

/** @param {Lit} s @returns {ClassSource} */
function stringSource(s) {
  const tokens = tokenize(s.text, s.start);
  return { tokens, views: [tokens], tag: -1, exempt: tokens.some((t) => ALLOW_LISTED.has(t.raw)) };
}

/**
 * @param {string} file repository-relative path, slash form
 * @param {string} src
 * @returns {Model}
 */
export function buildModel(file, src) {
  const kind = kindOf(file);
  /** @type {Model} */
  const m = { file, kind, src, clean: src, classSources: [], decls: [], cssRules: [], userStrings: [], copyStrings: [], labels: [], colorRegions: [], codeRegions: [], styleRanges: [] };
  if (kind === "css") {
    // A stylesheet is read twice. The first parse finds the declarations the
    // file OWNS (see isOwnedDeclaration); they are blanked, and the second parse
    // is what every rule reads, so a token definition is never a literal hit.
    const commentFree = blank(src, cssCommentRanges(src));
    const first = parseCss(commentFree, 0);
    const owned = [first.root, ...first.rules].flatMap((r) => r.decls).filter((d) => isOwnedDeclaration(d));
    m.clean = blank(commentFree, owned.map((d) => /** @type {[number, number]} */ ([d.start, d.end])));
    const parsed = parseCss(m.clean, 0);
    m.cssRules = parsed.rules;
    const all = [parsed.root, ...parsed.rules];
    m.decls.push(...all.flatMap((r) => r.decls));
    for (const d of m.decls) m.colorRegions.push({ text: d.value, start: d.start, css: true });
    for (const r of all) for (const a of r.applyList) addApplySource(m, a);
    return m;
  }
  if (kind === "ts") {
    const js = newJsOut();
    lexJs(src, 0, src.length, js, false);
    m.clean = blank(src, js.comments);
    for (const s of js.strings) {
      if (s.text.trim() === "") continue;
      m.classSources.push(stringSource(s));
      if (isTsCopy(src, s, file)) m.copyStrings.push(s);
      if (!TS_COPY_EXEMPT_FILES.test(file) && TS_LABEL_KEY.test(src.slice(Math.max(0, s.start - 80), s.start).replace(/["\x27`]$/, ""))) m.labels.push(s);
    }
    m.codeRegions.push([0, src.length]);
    return m;
  }
  const p = parseSvelte(src);
  const scriptJs = newJsOut();
  /** @type {[number, number][]} */
  const comments = [...p.htmlComments, ...p.markupJs.comments];
  for (const sc of p.scripts) lexJs(src, sc.start, sc.end, scriptJs, false);
  comments.push(...scriptJs.comments);
  for (const st of p.styles) for (const [a, b] of cssCommentRanges(src.slice(st.start, st.end))) comments.push([a + st.start, b + st.start]);
  m.clean = blank(src, comments);
  const clean = m.clean;

  for (const sc of p.scripts) m.codeRegions.push([sc.start, sc.end]);
  for (const st of p.styles) m.styleRanges.push([st.start, st.end]);
  for (const e of p.exprs) m.codeRegions.push([e.start, e.end]);
  for (const a of p.attrs) for (const sg of a.segs) if (sg.type === "expr") m.codeRegions.push([sg.start, sg.end]);

  // Class sources.
  /** @type {Map<number, ClassSource[]>} */
  const byTag = new Map();
  /** @param {ClassSource} cs */
  const addSource = (cs) => {
    m.classSources.push(cs);
    if (cs.tag >= 0) {
      const list = byTag.get(cs.tag) ?? [];
      list.push(cs);
      byTag.set(cs.tag, list);
    }
  };
  for (const a of p.attrs) {
    if (a.name.startsWith("class:")) {
      const tokens = tokenize(a.name.slice(6).replace(/\|.*$/, ""), a.nameStart + 6);
      addSource({ tokens, views: [tokens], tag: a.tag, exempt: false });
      continue;
    }
    if (!CLASS_ATTR.test(a.name)) continue;
    /** @type {Token[]} */
    const fixed = [];
    /** @type {Token[][]} */
    const lits = [];
    for (const sg of a.segs) {
      if (sg.type === "text") fixed.push(...tokenize(clean.slice(sg.start, sg.end), sg.start));
      else {
        const js = newJsOut();
        lexJs(clean, sg.start, sg.end, js, false);
        for (const s of js.strings) if (s.text.trim() !== "") lits.push(tokenize(s.text, s.start));
      }
    }
    const tokens = [...fixed, ...lits.flat()];
    const views = [fixed, ...lits.map((l) => [...fixed, ...l])];
    addSource({ tokens, views, tag: a.tag, exempt: false });
  }
  for (const s of scriptJs.strings) if (s.text.trim() !== "") addSource(stringSource(s));
  for (const list of byTag.values()) {
    const exempt = list.some((cs) => cs.tokens.some((t) => ALLOW_LISTED.has(t.raw)));
    for (const cs of list) cs.exempt = exempt;
  }

  // Declarations: <style> blocks, style="" attributes, style:prop directives.
  for (const st of p.styles) {
    const parsed = parseCss(clean.slice(st.start, st.end), st.start);
    m.cssRules.push(...parsed.rules);
    m.decls.push(...parsed.root.decls, ...parsed.rules.flatMap((r) => r.decls));
    for (const r of [parsed.root, ...parsed.rules]) for (const a of r.applyList) addApplySource(m, a);
    for (const r of [parsed.root, ...parsed.rules]) for (const d of r.decls) m.colorRegions.push({ text: d.value, start: d.start, css: true });
  }
  for (const a of p.attrs) {
    if (a.name === "style") {
      const text = a.segs.map((sg) => (sg.type === "text" ? clean.slice(sg.start, sg.end) : "var(--x)")).join("");
      const parsed = parseCss(text, a.nameStart);
      m.decls.push(...parsed.root.decls);
      for (const sg of a.segs) m.colorRegions.push({ text: clean.slice(sg.start, sg.end), start: sg.start, css: sg.type === "text" });
    } else if (a.name.startsWith("style:")) {
      const first = a.segs[0];
      const value = first ? clean.slice(first.start, first.end).trim() : "";
      const rule = newCssRule("", a.nameStart, null);
      m.decls.push({ prop: a.name.slice(6).toLowerCase(), value, start: a.nameStart, end: a.nameStart, rule });
    } else if (!DENY_COLOR_ATTRS.test(a.name)) {
      for (const sg of a.segs) m.colorRegions.push({ text: clean.slice(sg.start, sg.end), start: sg.start, css: false });
    }
  }
  for (const e of p.exprs) m.colorRegions.push({ text: clean.slice(e.start, e.end), start: e.start, css: false });

  // Rule 13 text: markup text nodes, display attributes, JS strings.
  // Rule 14 reads the markup ones: text a person sees, plus the string literals
  // of a mustache in a text position or in a display attribute (below), minus
  // the ones that are code (isCodeOperand), never `<script>` strings.
  for (const t of p.texts) {
    const lit = { start: t.start, end: t.end, text: clean.slice(t.start, t.end) };
    m.userStrings.push(lit);
    m.copyStrings.push(lit);
  }
  /** Mustache bodies whose string literals are copy: see below. @type {[number, number][]} */
  const copyRanges = [];
  // A mustache in a text position is copy unless it is a block or tag form
  // ({#if x === "a"}, {:else if}, {/each}, {@html}, {@const}): those hold code.
  for (const e of p.exprs) if (!/^\s*[#:/@]/.test(clean.slice(e.start, e.end))) copyRanges.push([e.start, e.end]);
  for (const a of p.attrs) {
    const component = /^[A-Z]/.test(a.tagName);
    if (CLASS_ATTR.test(a.name) || a.name === "style" || a.name.includes(":")) continue;
    if (!(DISPLAY_ATTRS.test(a.name) || (component && !NON_DISPLAY_PROPS.test(a.name)))) continue;
    // A test hook or other data-* attribute on a component is an identifier, not copy.
    const identifier = /^data-/i.test(a.name) && !/^data-(?:tip|tip-guide|tooltip)$/i.test(a.name);
    for (const sg of a.segs) {
      if (sg.type === "expr") {
        if (!identifier && !EVENT_PROP.test(a.name)) copyRanges.push([sg.start, sg.end]);
        continue;
      }
      const lit = { start: sg.start, end: sg.end, text: clean.slice(sg.start, sg.end) };
      m.userStrings.push(lit);
      if (!identifier) m.copyStrings.push(lit);
    }
  }
  for (const s of [...scriptJs.strings, ...p.markupJs.strings]) {
    if (CONSOLE_ARG.test(src.slice(Math.max(0, s.start - 40), s.start))) continue;
    m.userStrings.push(s);
  }
  for (const s of p.markupJs.strings) {
    if (CONSOLE_ARG.test(src.slice(Math.max(0, s.start - 40), s.start))) continue;
    const body = copyRanges.find(([a, b]) => s.start >= a && s.end <= b);
    if (body && !isCodeOperand(src, body, s)) m.copyStrings.push(s);
  }

  // Label positions, for the retired verbs of rule 14: the text a person reads
  // as the name of an action. See LABEL_TAGS.
  /** @type {Map<number, Attr[]>} */
  const attrsByTag = new Map();
  for (const a of p.attrs) attrsByTag.set(a.tag, [...(attrsByTag.get(a.tag) ?? []), a]);
  /** @type {Map<number, boolean>} */
  const labelMemo = new Map();
  /** @param {number} tag */
  const isLabelTag = (tag) => {
    if (tag < 0) return false;
    const known = labelMemo.get(tag);
    if (known !== undefined) return known;
    const attrs = attrsByTag.get(tag) ?? [];
    const role = attrs.find((a) => a.name === "role");
    const roleText = role ? role.segs.map((sg) => (sg.type === "text" ? clean.slice(sg.start, sg.end) : "")).join("").trim() : "";
    const hit =
      LABEL_TAGS.test(p.tags[tag]) ||
      LABEL_ROLES.test(roleText) ||
      (byTag.get(tag) ?? []).some((cs) => cs.tokens.some((t) => LABEL_CLASS_UTIL.test(t.util)));
    labelMemo.set(tag, hit);
    return hit;
  };
  /** @param {number} from @param {number} to @returns {Lit[]} the string literals of a mustache that are copy, not code */
  const copyIn = (from, to) =>
    p.markupJs.strings.filter((str) => str.start >= from && str.end <= to && !CONSOLE_ARG.test(src.slice(Math.max(0, str.start - 40), str.start)) && !isCodeOperand(src, [from, to], str));
  for (const t of p.texts) if (isLabelTag(t.parent)) m.labels.push({ start: t.start, end: t.end, text: clean.slice(t.start, t.end) });
  for (const e of p.exprs) if (isLabelTag(e.parent) && !/^\s*[#:/@]/.test(clean.slice(e.start, e.end))) m.labels.push(...copyIn(e.start, e.end));
  for (const a of p.attrs) {
    if (!LABEL_ATTRS.test(a.name) || !(isLabelTag(a.tag) || /^[A-Z]/.test(a.tagName))) continue;
    for (const sg of a.segs) {
      if (sg.type === "text") m.labels.push({ start: sg.start, end: sg.end, text: clean.slice(sg.start, sg.end) });
      else m.labels.push(...copyIn(sg.start, sg.end));
    }
  }
  return m;
}

/**
 * The elements whose text is an action label: a button, link, summary or option;
 * anything with an action role; a component named like a button or menu item; and
 * anything wearing one of the action utilities. Text in a paragraph or a heading
 * is prose and is not a label, whatever verb it starts with.
 */
const LABEL_TAGS = /^(?:button|a|summary|option|[A-Z]\w*(?:Button|MenuItem|Action|Link|Chip))$/;
const LABEL_ROLES = /^(?:button|menuitem|menuitemcheckbox|menuitemradio|tab|link|option)$/;
const LABEL_CLASS_UTIL = /^gp-(?:btn|icon-btn|menu-item|chip|seg-btn)$/;
const LABEL_ATTRS = /^(?:label|aria-label|title|data-tip|tooltip|text)$/;
const TS_LABEL_KEY = /(?:label|title|ariaLabel|actionLabel|buttonLabel|cta|paletteCommand|tooltip)["\x27]?\s*:\s*$/;

/** @param {Model} m @param {Lit} a one `@apply` value: its utilities are a class source like any other */
function addApplySource(m, a) {
  const tokens = tokenize(a.text, a.start);
  m.classSources.push({ tokens, views: [tokens], tag: -1, exempt: false });
}

/**
 * A declaration a stylesheet OWNS, which no literal rule reads:
 *   - a custom property (`--c-bg: rgb(...)`, `--gp-dur-fast: 140ms`,
 *     `--gp-ease-out: cubic-bezier(...)`, `@theme { --text-micro: 10px }`). The
 *     stylesheet is where tokens are defined, so the literal that defines one is
 *     the single permitted spelling of it; a rule that USES the token reads it
 *     through var(--x) and is still checked;
 *   - a mask declaration (`mask-image: linear-gradient(..., #000, transparent)`):
 *     the colour of a mask is its alpha channel, not a paint colour.
 * Nothing else is exempt, so `.gp-btn-success { background-image: linear-gradient(#34d399, ...) }`
 * and `transition: opacity 120ms` in a rule are findings.
 * @param {Decl} d
 */
function isOwnedDeclaration(d) {
  return d.prop.startsWith("--") || /^(?:-webkit-)?mask(?:-[a-z-]+)?$/.test(d.prop);
}

const DISPLAY_KEY = /(?:label|title|text|message|hint|description|heading|subtitle|caption|summary|tooltip|placeholder|headline|body|ariaLabel|actionLabel)["\x27]?\s*:\s*$/;
const NOT_COPY_CALL =
  /(?:invoke|listen|once|emit|emitTo|addEventListener|removeEventListener|querySelector(?:All)?|getElementById|closest|matches|getItem|setItem|removeItem|classList\.\w+|setAttribute|getAttribute|hasAttribute|removeAttribute|RegExp|CustomEvent|Event|startsWith|endsWith|includes|indexOf|split|join|replace|replaceAll|match|require|import|test|has|get|set|delete|resolve|dirname|basename|extname|relative|new URL|fetch|Symbol|Reflect\.\w+)\s*(?:<[^>()]*>)?\(\s*$/;
const NOT_COPY_KEY = /(?:keywords|aliases|synonyms|tags|selector|pattern|regex|command|cmd|argv|args|path|url|href|id|kind|type|key)["\x27]?\s*:\s*$/;
const TS_CODE_WORDS = /^(?:[.#/\[<@$~]|[a-z]+:\/\/|\w+\(|--)/;

/**
 * Whether a string literal of a `.ts` file is copy a person may read, for
 * rule 14. A `.ts` file has no markup, so the position cannot say it; the
 * literal itself and its left and right context have to:
 *   copy     a literal of two or more words (it holds whitespace and letters), or
 *            a single word that is the value of a display key (`label:`, `title:`,
 *            `description:`, `heading:`, `message:`, ...);
 *   not copy an identifier-shaped literal (no whitespace: a kind, id, path, key or
 *            event name), an object key (`"a b":`), an operand of ==, ===, !=, !==
 *            or `case`, an import specifier, the argument of a non-copy call
 *            (`invoke(`, `querySelector(`, `getItem(`, `classList.add(`, `new RegExp(`,
 *            `.includes(`, ...), a `console.*` argument, and anything that starts like
 *            a selector, path, URL or CSS custom property.
 * A template literal is read one segment at a time, so `MANVI is ${state}`
 * is a hit and `${state}` is not. The ban list itself (the vocabulary file) is
 * not copy: it names the words it forbids.
 * Blind spots: a single word with no display key (`return "Retry"`), copy built by
 * concatenation of single words, and copy in a call that is on the not-copy list.
 * @param {string} src @param {Lit} s @param {string} file
 */
function isTsCopy(src, s, file) {
  if (TS_COPY_EXEMPT_FILES.test(file)) return false;
  const text = s.text.trim();
  if (TS_CODE_WORDS.test(text) || !/[A-Za-z]{2}/.test(text)) return false;
  const before = src.slice(Math.max(0, s.start - 80), s.start).replace(/["\x27`]$/, "");
  const after = src.slice(s.end).replace(/^["\x27`]/, "");
  if (CONSOLE_ARG.test(src.slice(Math.max(0, s.start - 40), s.start))) return false;
  if (/(?:\bfrom|\bimport|\bcase)\s*$/.test(before) || NOT_COPY_CALL.test(before)) return false;
  if (/(?:===|!==|==|!=)\s*$/.test(before) || /^\s*(?:===|!==|==|!=)/.test(after.slice(0, 12))) return false;
  if (/^\s*:/.test(after) && /[{,]\s*$/.test(before)) return false;
  if (NOT_COPY_KEY.test(before)) return false;
  if (/\s/.test(text)) return true;
  return DISPLAY_KEY.test(before);
}

/** Files that hold the banned words as data: the ban list and the retired-verb list, and fixture data. */
const TS_COPY_EXEMPT_FILES = /^src\/lib\/ui\/copy\/vocabulary\.ts$|\.fixtures?\.ts$/;

/**
 * A snake_case word that is code, not a label, in a .ts string: a path segment or a
 * method (`go_orchestrator/cmd`, `conversation.send_message(`), a key=value pair of a log
 * line (`max_gap_ms=`), a code span (`python_files`), a directory name, or a line of source
 * (`from google_antigravity import Agent`). An identifier in plain prose is still a hit.
 * @param {string} text the whole string @param {number} at @param {string} word
 */
function isCodeShapedSnake(text, at, word) {
  const prev = text[at - 1] ?? "";
  const next = text[at + word.length] ?? "";
  return word === "node_modules" || "./`".includes(prev) && prev !== "" || "=(/".includes(next) && next !== "" || /^\s*(?:from|import)\s/.test(text);
}

/**
 * Whether the offset sits inside an `Error(` call on its own line. A message that names a
 * wire field (`new Error("health: audit_coverage was corrupt")`) is an assertion for a developer, and
 * the snake_case term is dropped there; every other term still applies to it.
 * @param {string} src @param {number} offset
 */
function inErrorCall(src, offset) {
  const lineStart = src.lastIndexOf("\n", offset - 1) + 1;
  return /\bError\([^;]*$/.test(src.slice(lineStart, offset));
}

/** `onTools={() => ...}`, `onclick={...}`: a handler is code (and `only` is not one). */
const EVENT_PROP = /^on(?:[A-Z:]|(?!ly$)[a-z]+$)/;

/**
 * Whether a string literal inside a mustache body is code rather than copy:
 * an operand of an equality test (`x === "allow_once"`, `"a" == x`), or an
 * argument of a call (`mapFailureMessage("no_cli")`, `openSetupWizard("devmap")`).
 * A parenthesised group (`(a ? "x" : "y")`) is not a call. Blind spot: a copy
 * string passed to a call (`plural(n, "file")`) is therefore not seen.
 * @param {string} src @param {[number, number]} body @param {Lit} s
 */
function isCodeOperand(src, body, s) {
  const before = src.slice(body[0], s.start).replace(/["'`]$/, "");
  const after = src.slice(s.end).replace(/^["'`]/, "");
  if (/(?:===|!==|==|!=)\s*$/.test(before)) return true;
  if (/^\s*(?:===|!==|==|!=)/.test(after.slice(0, 12))) return true;
  let depth = 0;
  for (let i = before.length - 1; i >= 0; i--) {
    const c = before[i];
    if (c === ")" || c === "]") depth++;
    else if (c === "(") {
      if (depth > 0) depth--;
      else return /[\w$)\]]\s*$/.test(before.slice(0, i));
    }
  }
  return false;
}

const CONSOLE_ARG = /console\.\w+\(\s*[`'"]?$/;
const DENY_COLOR_ATTRS = /^(?:href|xlink:href|id|for|name|src|action|form|list|target|popovertarget|aria-controls|aria-labelledby|aria-describedby|aria-owns|aria-activedescendant|style)$/;
const DISPLAY_ATTRS = /^(?:title|placeholder|alt|label|aria-label|aria-description|aria-placeholder|aria-valuetext|aria-roledescription|data-tip|data-tip-guide|data-tooltip|tooltip|text|message|hint|description|heading|subtitle|caption|summary|legend)$/i;
const NON_DISPLAY_PROPS = /^(?:id|href|src|name|key|type|role|rel|target|d|viewBox|for|pattern|autocomplete|inputmode|value|icon|variant|size|tone|kind|mode)$/;

// ---------------------------------------------------------------------------
// Rule helpers.
// ---------------------------------------------------------------------------

/**
 * Run `fn` over every token of every class source (once per token).
 * @param {Model} m @param {(t: Token, cs: ClassSource) => void} fn
 */
function eachToken(m, fn) {
  for (const cs of m.classSources) for (const t of cs.tokens) fn(t, cs);
}

const PALETTE = "slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose";
const PALETTE_UTIL = new RegExp(`^(?:text|bg|border(?:-[xytblrse])?|ring|ring-offset|outline|from|via|to|fill|stroke|divide|shadow|decoration|accent|caret|placeholder)-(?:${PALETTE})-(?:50|[1-9]00|950)$`);
const COLOR_PREFIX = /^(text|bg|border|ring|outline|from|via|to|fill|stroke|divide|decoration|placeholder|caret|accent)-(.+)$/;
/** @type {Record<string, RegExp>} */
const NON_COLOR_REST = {
  text: /^(?:xs|sm|base|lg|[2-9]?xl|micro|meta|left|right|center|justify|start|end|wrap|nowrap|balance|pretty|ellipsis|clip|\[.*)$/,
  bg: /^(?:none|cover|contain|center|top|bottom|left|right|fixed|local|scroll|repeat.*|no-repeat|clip-.*|origin-.*|blend-.*|gradient-.*|linear-.*|radial.*|conic.*|auto|\[.*)$/,
  border: /^(?:\d+|[xytblrse](?:-\d+)?|solid|dashed|dotted|double|none|hidden|collapse|separate|spacing.*|\[.*)$/,
  ring: /^(?:\d+|inset|offset-\d+|\[.*)$/,
  outline: /^(?:\d+|none|dashed|dotted|double|hidden|offset-\d+|\[.*)$/,
  from: /^(?:\d+%|\[.*)$/,
  via: /^(?:\d+%|\[.*)$/,
  to: /^(?:\d+%|\[.*)$/,
  fill: /^\[.*$/,
  stroke: /^(?:\d+|\[.*)$/,
  divide: /^(?:[xy](?:-\d+|-reverse)?|solid|dashed|dotted|double|none|\[.*)$/,
  decoration: /^(?:solid|double|dotted|dashed|wavy|auto|from-font|\d+|\[.*)$/,
  placeholder: /^\[.*$/,
  caret: /^\[.*$/,
  accent: /^(?:auto|\[.*)$/,
};

/** @param {string} util */
function isColorUtil(util) {
  const m = COLOR_PREFIX.exec(util);
  if (!m) return false;
  return !NON_COLOR_REST[m[1]].test(m[2]);
}

const BANNED_TRANSITION_PROPS = new Set([
  "all", "width", "height", "min-width", "max-width", "min-height", "max-height", "top", "left", "right", "bottom", "inset",
  "margin", "margin-top", "margin-bottom", "margin-left", "margin-right", "padding", "padding-top", "padding-bottom", "padding-left",
  "padding-right", "filter", "box-shadow", "backdrop-filter", "-webkit-backdrop-filter", "border-width", "grid-template-columns",
  "grid-template-rows", "flex-basis", "gap", "block-size", "inline-size", "margin-block", "margin-inline", "padding-block",
  "padding-inline", "inset-block", "inset-inline",
]);

/**
 * An arbitrary-property class, `[transition:height_200ms]` or
 * `[animation:foo_1s]`: Tailwind writes the underscore for a space.
 * @param {string} util @returns {{ prop: string, value: string } | null}
 */
function arbitraryProperty(util) {
  const hit = /^\[([a-z-]+):(.+)\]$/.exec(util);
  return hit ? { prop: hit[1], value: hit[2].replace(/_/g, " ") } : null;
}

/** @param {string} value an arbitrary class value, `_` standing for a space */
function arbitraryHasMotionLiteral(value) {
  const spaced = value.replace(/_/g, " ");
  return hasTimeLiteral(spaced) || /cubic-bezier\(|(?<![\w-])(?:ease|ease-in|ease-out|ease-in-out|linear)(?![\w-])/.test(spaced);
}

/** @param {string} value comma-separated transition-property or shorthand list @returns {string[]} */
function transitionedProperties(value) {
  /** @type {string[]} */
  const parts = [];
  let depth = 0;
  let last = 0;
  for (let i = 0; i < value.length; i++) {
    if (value[i] === "(") depth++;
    else if (value[i] === ")") depth = Math.max(0, depth - 1);
    else if (value[i] === "," && depth === 0) {
      parts.push(value.slice(last, i));
      last = i + 1;
    }
  }
  parts.push(value.slice(last));
  return parts.map((s) => s.trim().split(/\s+/)[0].toLowerCase()).filter((s) => s !== "");
}

/** @param {string} value */
function hasTimeLiteral(value) {
  return /(?<![\w.-])\d*\.?\d+m?s\b/.test(value.replace(/var\([^)]*\)/g, ""));
}

const HEX = "(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})";
const HEX_CSS = new RegExp(`(?<![\\w&#/.-])(?<!url\\()#${HEX}(?![\\w-])`, "g");
const HEX_WHOLE = new RegExp(`^\\s*#${HEX}\\s*$`);
const HEX_QUOTED = new RegExp(`(?<=["'])#${HEX}(?=["'])`, "g");
const HEX_ARBITRARY = new RegExp(`(?<=\\[)#${HEX}(?=[\\]/_])`, "g");
const COLOR_FN = /(?<![\w-])(?:rgb|rgba|hsl|hsla)\(\s*\d/g;

/**
 * The one layout transition the design language allows (DESIGN_LANGUAGE section 8.5):
 * the docked sidebar slides its width once, off while it is dragged. Rule 9 reads
 * stylesheets, so this utility is named here rather than allow-listed, which would
 * excuse every other layout transition in the same file.
 */
const LAYOUT_TRANSITION_OWNERS = new Set(["gp-slide-docked"]);

/** @param {CssRule} r the utility a rule defines (`@utility gp-x`), or the empty string */
function utilityName(r) {
  return r.selector.startsWith("@utility ") ? r.selector.slice(9).trim() : "";
}

const OWNERS_OF_MOTION = /^src\/lib\/(?:motion\/|ui\/transitions\.ts$)/;

/**
 * `BANNED_UI_TERMS` with the global flag `matchAll` needs (the vocabulary's
 * patterns carry none, so they stay stateless for every other reader).
 */
const GLOBAL_BANNED_TERMS = BANNED_UI_TERMS.map((t) => ({
  term: t.term,
  use: t.use,
  pattern: new RegExp(t.pattern.source, t.pattern.flags.includes("g") ? t.pattern.flags : `${t.pattern.flags}g`),
}));

/**
 * `RETIRED_VERB_TERMS` anchored to the start of a label, so "Retry search" and
 * "Reload" are hits and "Nothing to reload" is not: a verb is retired as the
 * name of an action, and only its leading position says that it is the action.
 */
const LEADING_RETIRED_VERBS = RETIRED_VERB_TERMS.map((t) => ({
  term: t.term,
  use: t.use,
  pattern: new RegExp(`^\\s*(?:${t.pattern.source})`, t.pattern.flags.replace("g", "")),
}));

/**
 * Rule 14 false positives that are not a glossary breach, accepted by shape:
 *   - "project" qualified as the vendor's own term: "Firebase project", "Google Cloud
 *     project", "project alias", "project ID" (in Firebase and Google Cloud a project is
 *     not a repository);
 *   - "repo" as a URL path segment, the placeholder "github.com/owner/repo.git", which
 *     is GitHub's own convention.
 * A bare "project" or "repo" in prose still fails. A file whose bare uses are all the
 * vendor's word is listed in design-lint.config.json (allow, rule 14, with a term and a reason).
 * @param {string} term @param {string} text @param {number} at @param {number} len
 */
function isVendorTermUse(term, text, at, len) {
  const before = text.slice(Math.max(0, at - 14), at);
  const after = text.slice(at + len, at + len + 8);
  if (term === "project") return /(?:Firebase|Google Cloud|GCP) $/i.test(before) || /^ (?:alias(?:es)?|IDs?)\b/i.test(after);
  if (term === "repo") return before.endsWith("/") && (/^\.git\b/.test(after) || after === "" || after.startsWith("/"));
  return false;
}

// ---------------------------------------------------------------------------
// Rule 15 helpers.
// ---------------------------------------------------------------------------

/** @param {string} sel @param {string} sep @returns {string[]} */
function splitTop(sel, sep) {
  /** @type {string[]} */
  const out = [];
  let depth = 0;
  let last = 0;
  for (let i = 0; i < sel.length; i++) {
    const ch = sel[i];
    if (ch === "(" || ch === "[") depth++;
    else if (ch === ")" || ch === "]") depth = Math.max(0, depth - 1);
    else if (ch === sep && depth === 0) {
      out.push(sel.slice(last, i));
      last = i + 1;
    }
  }
  out.push(sel.slice(last));
  return out;
}

/** @param {string} complex one complex selector @returns {string} the compound after the last combinator */
function lastCompound(complex) {
  const s = complex.trim();
  let depth = 0;
  let last = 0;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === "(" || ch === "[") depth++;
    else if (ch === ")" || ch === "]") depth = Math.max(0, depth - 1);
    else if (depth === 0 && /[\s>+~]/.test(ch)) last = i + 1;
  }
  return s.slice(last);
}

/**
 * @param {string} selector
 * @returns {{ classes: string[], patterns: string[] }}
 */
function subjectClasses(selector) {
  /** @type {string[]} */
  const classes = [];
  /** @type {string[]} */
  const patterns = [];
  for (const part of splitTop(selector, ",")) {
    let c = lastCompound(part);
    for (const m of c.matchAll(/\[class\*=["']([^"']+)["']\]/g)) patterns.push(m[1]);
    c = c
      .replace(/\[[^\]]*\]/g, "")
      .replace(/:not\((?:[^()]|\([^)]*\))*\)/g, "")
      .replace(/::?[\w-]+\((?:[^()]|\([^)]*\))*\)/g, (hit) => (/^:(?:is|where)\(/.test(hit) ? hit : ""));
    for (const m of c.matchAll(/\.(-?[_a-zA-Z][\w-]*)/g)) classes.push(m[1]);
  }
  return { classes, patterns };
}

/** @param {CssRule} r @returns {CssRule[]} the rule and its ancestors */
function chain(r) {
  /** @type {CssRule[]} */
  const out = [];
  for (let c = /** @type {CssRule | null} */ (r); c; c = c.parent) out.push(c);
  return out;
}

/** @param {Decl} d */
function animatesDecl(d) {
  const v = d.value.trim().toLowerCase();
  if (d.prop === "animation" || d.prop === "transition") return !/^(?:none|0s?)\b/.test(v) && v !== "";
  if (d.prop === "animation-name" || d.prop === "transition-property") return v !== "none";
  return false;
}

/** @param {string} applied */
function applyAnimates(applied) {
  return applied.split(/\s+/).some((t) => {
    const util = t.split(":").pop() ?? "";
    return (/^transition(?:-|$)/.test(util) && util !== "transition-none") || (/^animate-/.test(util) && util !== "animate-none");
  });
}

// ---------------------------------------------------------------------------
// The registry. Rule 14 (banned copy terms) is one more entry here.
// ---------------------------------------------------------------------------

/** @type {Rule[]} */
export const RULES = [
  {
    id: 1,
    name: "type-scale",
    summary: "text-[Npx] and font-size: Npx literal font sizes",
    svelte: true,
    ts: true,
    css: true,
    run(m, report) {
      eachToken(m, (t) => {
        if (/^text-\[\d+(?:\.\d+)?px\]$/.test(t.util)) report(t.start, `${t.util}: use text-micro, text-meta, text-xs, text-sm, text-base or text-lg`);
      });
      for (const d of m.decls) {
        if (d.prop === "font-size" && /^\d+(?:\.\d+)?px(?:\s*!important)?$/.test(d.value)) report(d.start, `font-size: ${d.value}: use the type scale (text-micro, text-meta, text-xs, text-sm, text-base, text-lg)`);
      }
    },
  },
  {
    id: 2,
    name: "weight-and-case",
    summary: "font-bold / weight 650 / uppercase with tracking",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      eachToken(m, (t) => {
        if (/^font-(?:thin|extralight|light|bold|extrabold|black)$/.test(t.util)) report(t.start, `${t.util}: use font-normal, font-medium or font-semibold`);
      });
      for (const cs of m.classSources) {
        for (const view of cs.views) {
          const upper = view.find((t) => t.util === "uppercase");
          if (upper && view.some((t) => t.util.startsWith("tracking-"))) {
            report(upper.start, "uppercase with tracking-*: section labels use gp-label (sentence case)");
            break;
          }
        }
      }
      for (const d of m.decls) {
        if (d.prop !== "font-weight") continue;
        const v = d.value.toLowerCase().replace(/\s*!important$/, "");
        if (/^(?:400|500|600|normal|inherit|initial|unset)$/.test(v) || v.startsWith("var(")) continue;
        report(d.start, `font-weight: ${v}: only 400, 500 and 600 exist`);
      }
      /** @type {Set<CssRule>} */
      const reported = new Set();
      for (const d of m.decls) {
        if (d.prop === "text-transform" && /^uppercase\b/i.test(d.value) && !reported.has(d.rule) && d.rule.decls.some((x) => x.prop === "letter-spacing")) {
          reported.add(d.rule);
          report(d.start, "text-transform: uppercase with letter-spacing: use gp-label");
        }
      }
    },
  },
  {
    id: 3,
    name: "palette-colors",
    summary: "Tailwind palette colors and dark: color pairs",
    svelte: true,
    ts: true,
    css: true,
    run(m, report) {
      eachToken(m, (t) => {
        if (PALETTE_UTIL.test(t.util)) report(t.start, `${t.raw}: Tailwind palette color, use a role token (text-ok/warn/danger/info/unknown, text-accent, text-textMuted)`);
        else if (t.variants.includes("dark") && isColorUtil(t.util)) report(t.start, `${t.raw}: dark: color pair, role tokens already follow the theme`);
      });
    },
  },
  {
    id: 4,
    name: "color-literals",
    summary: "#hex / rgb() literals in markup, <style> and the stylesheets",
    svelte: true,
    ts: false,
    css: true,
    run(m, report) {
      for (const r of m.colorRegions) {
        /** @type {RegExp[]} */
        const res = r.css ? [HEX_CSS] : [HEX_QUOTED, HEX_ARBITRARY];
        for (const re of res) {
          for (const hit of r.text.matchAll(re)) {
            const before = r.text.slice(Math.max(0, (hit.index ?? 0) - 40), hit.index);
            if (!r.css && /(?:querySelector(?:All)?|getElementById|closest|matches|scrollTo)\W*$/.test(before)) continue;
            report(r.start + (hit.index ?? 0), `${hit[0]}: color literal, use a --c-* token`);
          }
        }
        if (!r.css && HEX_WHOLE.test(r.text)) report(r.start + r.text.indexOf("#"), `${r.text.trim()}: color literal, use a --c-* token`);
        for (const hit of r.text.matchAll(COLOR_FN)) report(r.start + (hit.index ?? 0), `${hit[0]}…): color function literal, use a --c-* token`);
      }
    },
  },
  {
    id: 5,
    name: "radius",
    summary: "rounded-[...] / rounded-3xl",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      eachToken(m, (t) => {
        const arb = /^rounded(?:-[a-z]{1,2})?-\[(.+)\]$/.exec(t.util);
        if (arb && !arb[1].startsWith("var(--gp-r-")) report(t.start, `${t.util}: radius picked by eye, use --gp-r-* through the owning component`);
        else if (/^rounded(?:-[a-z]{1,2})?-3xl$/.test(t.util)) report(t.start, `${t.util}: use --gp-r-* through the owning component`);
      });
    },
  },
  {
    id: 6,
    name: "z-index",
    summary: "z-N / z-[N] / literal z-index",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      eachToken(m, (t) => {
        if (/^z-(?:[1-9]\d*|\[.+\])$/.test(t.util)) report(t.start, `${t.util}: use LAYERS from src/lib/ui/layers.ts`);
      });
      for (const d of m.decls) {
        if (d.prop === "z-index" && /^-?[1-9]\d*(?:\s*!important)?$/.test(d.value)) report(d.start, `z-index: ${d.value}: use LAYERS from src/lib/ui/layers.ts`);
      }
    },
  },
  {
    id: 7,
    name: "hairlines",
    summary: "border-t/b/l/r and divide-* region separators",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      eachToken(m, (t, cs) => {
        if (cs.exempt) return;
        const side = /^border-([tblrxyse])(?:-(\d+|\[\d+(?:px)?\]))?$/.exec(t.util);
        const divide = /^divide-[xy](?:-(\d+))?$/.exec(t.util);
        const width = side ? side[2] : divide ? divide[1] : undefined;
        if ((side || divide) && !/^(?:0|\[0(?:px)?\])$/.test(width ?? "")) report(t.start, `${t.util}: separate regions with spacing, a tonal step or [data-gp-fade], not a hairline`);
      });
    },
  },
  {
    id: 8,
    name: "border-opacity",
    summary: "border opacity other than 35 / 65 / 90",
    svelte: true,
    ts: false,
    css: true,
    run(m, report) {
      const STOPS = new Set(["35", "65", "90", "[0.35]", "[.35]", "[0.65]", "[.65]", "[0.9]", "[.9]", "[0.90]"]);
      eachToken(m, (t) => {
        if (t.opacity === null) return;
        if (!/^(?:border(?:-[xytblrse])?-[a-z][\w-]*|divide-[a-z][\w-]*)$/.test(t.util)) return;
        if (/^(?:border|divide)-(?:solid|dashed|dotted|double|none|hidden|collapse|separate|spacing.*|x|y)$/.test(t.util)) return;
        if (!STOPS.has(t.opacity)) report(t.start, `${t.util}/${t.opacity}: border opacity stops are --gp-edge-soft (35), --gp-edge (65) and --gp-edge-strong (90)`);
      });
      for (const d of m.decls) {
        if (!/^(?:border|outline)/.test(d.prop)) continue;
        for (const hit of d.value.matchAll(/\/\s*(\d*\.?\d+%?)\s*\)/g)) {
          const v = Number.parseFloat(hit[1]) / (hit[1].endsWith("%") ? 100 : 1);
          if (![0.35, 0.65, 0.9, 1].includes(v)) report(d.start, `${d.prop}: opacity ${hit[1]}: border opacity stops are 0.35, 0.65 and 0.9`);
        }
      }
    },
  },
  {
    id: 9,
    name: "layout-transitions",
    summary: "transition-all, non-compositor transitions and arbitrary animations",
    svelte: true,
    ts: true,
    css: true,
    run(m, report) {
      eachToken(m, (t) => {
        if (t.util === "transition-all" || t.util === "transition-shadow") {
          report(t.start, `${t.util}: animate transform and opacity (colors only at --gp-dur-instant)`);
          return;
        }
        if (/^animate-\[.+\]$/.test(t.util)) {
          report(t.start, `${t.util}: arbitrary animation escapes the reduced-motion lists (rule 15); use a named gp-* animation utility from src/app.css`);
          return;
        }
        const arb = /^transition-\[(.+)\]$/.exec(t.util);
        if (arb) {
          const bad = arb[1].split(/[,_]/).map((s) => s.trim().toLowerCase()).filter((s) => BANNED_TRANSITION_PROPS.has(s));
          if (bad.length > 0) report(t.start, `${t.util}: transitions ${bad.join(", ")}; animate transform and opacity only`);
          return;
        }
        const prop = arbitraryProperty(t.util);
        if (!prop) return;
        if (/^animation(?:-name)?$/.test(prop.prop)) {
          if (prop.value.trim() === "none") return;
          report(t.start, `${t.util}: arbitrary animation escapes the reduced-motion lists (rule 15); use a named gp-* animation utility from src/app.css`);
        } else if (prop.prop === "transition" || prop.prop === "transition-property") {
          const bad = transitionedProperties(prop.value).filter((p) => BANNED_TRANSITION_PROPS.has(p));
          if (bad.length > 0) report(t.start, `${t.util}: transitions ${bad.join(", ")}; animate transform and opacity only`);
        }
      });
      for (const d of m.decls) {
        if (d.prop !== "transition" && d.prop !== "transition-property") continue;
        if (chain(d.rule).some((r) => LAYOUT_TRANSITION_OWNERS.has(utilityName(r)))) continue;
        const bad = transitionedProperties(d.value).filter((p) => BANNED_TRANSITION_PROPS.has(p));
        if (bad.length > 0) report(d.start, `${d.prop} names ${bad.join(", ")}; animate transform and opacity only`);
      }
    },
  },
  {
    id: 10,
    name: "motion-literals",
    summary: "duration / easing literals",
    svelte: true,
    ts: true,
    css: true,
    run(m, report) {
      if (OWNERS_OF_MOTION.test(m.file)) return;
      eachToken(m, (t) => {
        const arb = /^(?:animate|transition)-\[(.+)\]$/.exec(t.util);
        const prop = arbitraryProperty(t.util);
        const value = arb ? arb[1] : prop && /^(?:animation|transition)(?:-[a-z]+)?$/.test(prop.prop) ? prop.value : null;
        if (value !== null && arbitraryHasMotionLiteral(value)) report(t.start, `${t.util}: duration or easing literal inside an arbitrary value, use --gp-dur-* / --gp-ease-* / src/lib/motion/tokens.ts`);
      });
      if (m.kind !== "ts") {
        eachToken(m, (t) => {
          if (/^duration-(?:\d+|\[.+\])$/.test(t.util)) report(t.start, `${t.util}: duration literal, use --gp-dur-* / src/lib/motion/tokens.ts`);
          else if (/^ease-(?:in|out|in-out|linear|\[.+\])$/.test(t.util)) report(t.start, `${t.util}: easing literal, use --gp-ease-* / src/lib/motion/tokens.ts`);
        });
        for (const d of m.decls) {
          if (/^(?:transition|transition-duration|animation|animation-duration)$/.test(d.prop) && hasTimeLiteral(d.value)) report(d.start, `${d.prop}: ${d.value}: time literal, use var(--gp-dur-*)`);
        }
      }
      for (const [s, e] of m.codeRegions) {
        const code = m.clean.slice(s, e);
        for (const hit of code.matchAll(/(?<![\w$.-])duration\s*:\s*(?:motionDuration\(\s*)?(?!0\b)\d/g)) report(s + (hit.index ?? 0), "duration literal in transition params: use src/lib/motion/tokens.ts");
        for (const hit of code.matchAll(/(?<![\w$.])motionDuration\(\s*(?!0\b)\d/g)) report(s + (hit.index ?? 0), "motionDuration(N): duration literal, use src/lib/motion/tokens.ts");
      }
      for (const hit of m.clean.matchAll(/cubic-bezier\(/g)) report(hit.index ?? 0, "cubic-bezier(...): easing literal, use --gp-ease-* (baked by scripts/bake-springs.mjs)");
    },
  },
  {
    id: 11,
    name: "backdrop-blur",
    summary: "backdrop-filter / backdrop-blur outside the float rule",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      eachToken(m, (t) => {
        if (/^backdrop-(?:blur|brightness|contrast|grayscale|hue-rotate|invert|opacity|saturate|sepia|filter)(?:-.+)?$/.test(t.util)) report(t.start, `${t.util}: only the float tier blurs, wear shadow-pop/shadow-float or gp-menu`);
      });
      for (const d of m.decls) {
        if (/^(?:-webkit-)?backdrop-filter$/.test(d.prop) && !/^none\b/i.test(d.value)) report(d.start, `${d.prop}: only the float rule in app.css blurs`);
      }
    },
  },
  {
    id: 12,
    name: "widget-dialects",
    summary: "scoped <style> class that is a button, link, chip, pill, toggle or tab",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      for (const r of m.cssRules) {
        if (r.at || chain(r).some((a) => /^@(?:-\w+-)?keyframes\b/.test(a.selector))) continue;
        const sel = r.selector.replace(/:not\((?:[^()]|\([^)]*\))*\)/g, (s) => " ".repeat(s.length));
        for (const hit of sel.matchAll(/\.(-?[_a-zA-Z][\w-]*)/g)) {
          if (!hit[1].startsWith("gp-") && isWidgetClass(hit[1])) report(r.selStart + (hit.index ?? 0), `.${hit[1]}: scoped widget dialect, use the owner in UI_RULES section 2`);
        }
      }
    },
  },
  {
    id: 13,
    name: "ascii-ellipsis",
    summary: '"..." in a user-visible string',
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      for (const s of m.userStrings) {
        for (const hit of s.text.matchAll(/(?<!\.)\.\.\.(?!\.)/g)) {
          const at = s.start + (hit.index ?? 0);
          // A git range such as main...HEAD or `${base}...${head}` is not prose.
          if (/[\w}]/.test(m.src[at - 1] ?? "") && /[\w$]/.test(m.src[at + 3] ?? "")) continue;
          // The separator argument of split/join/indexOf/... is code, not copy.
          if (s.text === "..." && /\.(?:split|join|indexOf|lastIndexOf|includes|startsWith|endsWith|replace|replaceAll)\(\s*$/.test(m.src.slice(Math.max(0, s.start - 40), s.start - 1))) continue;
          report(at, "'...' in user-visible text: use the single character '…'");
        }
      }
    },
  },
  {
    id: 14,
    name: "banned-ui-terms",
    summary: "banned product vocabulary, and retired verbs used as a label, in user-visible text",
    svelte: true,
    ts: true,
    css: false,
    run(m, report) {
      for (const s of m.copyStrings) {
        for (const term of GLOBAL_BANNED_TERMS) {
          for (const hit of s.text.matchAll(term.pattern)) {
            if (isVendorTermUse(term.term, s.text, hit.index ?? 0, hit[0].length)) continue;
            if (m.kind === "ts" && term.term === "snake_case identifier" && (inErrorCall(m.src, s.start) || isCodeShapedSnake(s.text, hit.index ?? 0, hit[0]))) continue;
            report(s.start + (hit.index ?? 0), `"${hit[0]}" (${term.term}): say "${term.use}"`, term.term);
          }
        }
      }
      for (const s of m.labels) {
        for (const verb of LEADING_RETIRED_VERBS) {
          const hit = verb.pattern.exec(s.text);
          if (hit) report(s.start + (hit.index ?? 0) + hit[0].length - hit[0].trimStart().length, `"${hit[0].trim()}" (retired verb ${verb.term}) as a label: say "${verb.use}"`, verb.term);
        }
      }
    },
  },
  {
    id: 15,
    name: "reduced-motion-lists",
    summary: "animated class missing from the reduced-motion lists",
    svelte: false,
    ts: false,
    css: true,
    run(m, report) {
      if (m.file !== APP_CSS) return;
      const rules = m.cssRules;
      /** @type {Map<string, number>} */ const reduceMedia = new Map();
      /** @type {Map<string, number>} */ const reduceAttr = new Map();
      /** @type {string[]} */ const mediaPats = [];
      /** @type {string[]} */ const attrPats = [];
      const inReduceMedia = (/** @type {CssRule} */ r) => chain(r).some((a) => /^@media[^{]*prefers-reduced-motion\s*:\s*reduce/.test(a.selector));
      const inNoPref = (/** @type {CssRule} */ r) => chain(r).some((a) => /^@media[^{]*prefers-reduced-motion\s*:\s*no-preference/.test(a.selector));
      const inKeyframes = (/** @type {CssRule} */ r) => chain(r).some((a) => /^@(?:-\w+-)?keyframes\b/.test(a.selector));
      const ATTR_LIST = /^html\[data-motion=["']reduced["']\]/;
      /** @param {CssRule} r */
      const killsMotion = (r) => r.decls.some((d) => (d.prop === "animation" || d.prop === "transition") && /\bnone\b/.test(d.value));
      for (const r of rules) {
        if (r.at || inKeyframes(r) || !killsMotion(r)) continue;
        const selector = r.selector.trim();
        const isMedia = inReduceMedia(r);
        const isAttr = !isMedia && ATTR_LIST.test(selector) && !chain(r).some((a) => a.at);
        if (!isMedia && !isAttr) continue;
        const { classes, patterns } = subjectClasses(selector);
        const map = isMedia ? reduceMedia : reduceAttr;
        for (const c of classes) if (!map.has(c)) map.set(c, r.selStart);
        (isMedia ? mediaPats : attrPats).push(...patterns);
      }
      /** @param {string} c @param {Map<string, number>} map @param {string[]} pats */
      const covered = (c, map, pats) => map.has(c) || pats.some((p) => c.includes(p));
      /** @type {Set<string>} */
      const seen = new Set();
      for (const r of rules) {
        if ((r.at && !r.selector.startsWith("@utility ")) || inKeyframes(r) || inReduceMedia(r) || ATTR_LIST.test(r.selector.trim())) continue;
        if (inNoPref(r) && r.selector.includes(':not([data-motion="reduced"])')) continue;
        const animates = r.decls.some(animatesDecl) || r.applies.some(applyAnimates);
        if (!animates) continue;
        const owner = chain(r).find((a) => a.selector.startsWith("@utility "));
        /** @type {string[]} */
        const classes = owner ? [owner.selector.slice(9).trim()] : subjectClasses(r.selector).classes;
        for (const c of classes) {
          if (!covered(c, reduceMedia, mediaPats) && !seen.has(`m:${c}`)) {
            seen.add(`m:${c}`);
            report(r.selStart, `.${c} animates but is not in the @media (prefers-reduced-motion: reduce) list`);
          }
          if (!covered(c, reduceAttr, attrPats) && !seen.has(`a:${c}`)) {
            seen.add(`a:${c}`);
            report(r.selStart, `.${c} animates but is not in the html[data-motion="reduced"] list`);
          }
        }
      }
      for (const [c, at] of reduceMedia) if (!reduceAttr.has(c)) report(at, `.${c} is in the prefers-reduced-motion list but not the data-motion list`);
      for (const [c, at] of reduceAttr) if (!reduceMedia.has(c)) report(at, `.${c} is in the data-motion list but not the prefers-reduced-motion list`);
    },
  },
  {
    id: 16,
    name: "heading-separator",
    summary: "gp-separator directly under a section heading, which already separates its group",
    svelte: true,
    ts: false,
    css: false,
    run(m, report) {
      // A heading is an <h1>-<h6>, or an element carrying `font-semibold` or
      // `gp-label` whose own content holds no element of the same name. The
      // separator must follow its closing tag with nothing between: a
      // separator that divides one group from the next (a menu group, a
      // heading that comes after it) is not this.
      const SEPARATOR = String.raw`\s*<(?:div|span|hr)\b[^>]*\bgp-separator\b`;
      const patterns = [
        new RegExp(String.raw`<(h[1-6])\b[^>]*>[\s\S]*?</\1>${SEPARATOR}`, "g"),
        new RegExp(String.raw`<(div|span|p)\b[^>]*\b(?:font-semibold|gp-label)\b[^>]*>(?:(?!</?\1\b)[\s\S])*</\1>${SEPARATOR}`, "g"),
      ];
      for (const re of patterns) {
        for (const hit of m.src.matchAll(re)) {
          report((hit.index ?? 0) + hit[0].lastIndexOf("<"), "gp-separator under a heading: the heading already separates its group, so use spacing (mb-2 on the heading row)");
        }
      }
    },
  },
  {
    id: 17,
    name: "undefined-gp-class",
    summary: "a gp-* class used in markup or @apply that no stylesheet defines",
    svelte: true,
    ts: false,
    css: true,
    runRepo(models, report) {
      const defined = gpDefinitions(models);
      for (const u of gpUsages(models)) {
        if (!defined.has(u.name)) report(u.file, u.start, `${u.name}: no stylesheet defines this class (no @utility, no .${u.name} rule), so it styles nothing`, u.name);
      }
    },
  },
  {
    id: 18,
    name: "unused-gp-class",
    summary: "a gp-* class a stylesheet defines that nothing uses",
    svelte: true,
    ts: true,
    css: true,
    runRepo(models, report, extraText) {
      const used = gpMentions(models, extraText);
      for (const [name, site] of gpDefinitions(models)) {
        if (used.names.has(name) || used.prefixes.some((p) => name.startsWith(p))) continue;
        report(site.file, site.offset, `${name}: defined but no markup, script or @apply uses it; delete it or use it`, name);
      }
    },
  },
];

// ---------------------------------------------------------------------------
// Rules 17 and 18: the gp-* class vocabulary, which only the whole tree can see.
// ---------------------------------------------------------------------------

const GP_CLASS_NAME = /^gp-[a-z0-9](?:[\w-]*[a-z0-9_])?$/;

/**
 * Every gp-* class the stylesheets define, with the first place each is defined:
 * `@utility gp-x`, and any `.gp-x` in a selector of src/app.css, another
 * stylesheet or a component's own `<style>` (`:global(.gp-x)` included). A class
 * named only inside `:not()` is not defined by that. A class that appears only as
 * the context of another selector (`.gp-menu .gp-menu-item`) counts as defined:
 * the rule is written, so the author meant it.
 * @param {Model[]} models @returns {Map<string, { file: string, offset: number }>}
 */
function gpDefinitions(models) {
  /** @type {Map<string, { file: string, offset: number }>} */
  const defs = new Map();
  for (const m of models) {
    for (const r of m.cssRules) {
      const util = utilityName(r);
      if (GP_CLASS_NAME.test(util) && !defs.has(util)) defs.set(util, { file: m.file, offset: r.selStart });
      if (r.at) continue;
      const sel = r.selector.replace(/:not\((?:[^()]|\([^)]*\))*\)/g, (hit) => " ".repeat(hit.length));
      for (const hit of sel.matchAll(/\.(gp-[\w-]+)/g)) {
        if (GP_CLASS_NAME.test(hit[1]) && !defs.has(hit[1])) defs.set(hit[1], { file: m.file, offset: r.selStart + (hit.index ?? 0) });
      }
    }
  }
  return defs;
}

/**
 * The gp-* classes a file applies in a position that is certainly a class: a
 * `class`, `*Class` or `class:name` attribute of an element or component, and an
 * `@apply`. A class string held in a script or a .ts file is not read here: the
 * same literal may be a storage key or an event name (rule 18 reads those through
 * gpMentions, which is allowed to be generous).
 * @param {Model[]} models @returns {{ file: string, start: number, name: string }[]}
 */
function gpUsages(models) {
  /** @type {{ file: string, start: number, name: string }[]} */
  const out = [];
  for (const m of models) {
    if (m.kind === "ts") continue;
    for (const cs of m.classSources) {
      if (cs.tag < 0 && m.kind === "svelte") continue;
      for (const t of cs.tokens) if (GP_CLASS_NAME.test(t.util)) out.push({ file: m.file, start: t.start, name: t.util });
    }
  }
  return out;
}

/**
 * Where a gp-* name is mentioned at all, for rule 18, which must be generous (a
 * class toggled from a script, queried by a test hook or used in index.html is not
 * unused): every `gp-name` word in markup, scripts, strings and .ts code outside
 * `<style>` bodies and comments, in an @apply, and in `extraText` (index.html and the
 * files under public/). A word that ends in a hyphen (`gp-seg-${tone}`, `gp-btn-{variant}`) is
 * a prefix and keeps every class that starts with it alive. Blind spot: a class
 * built from pieces that share no literal prefix (`"gp-" + a`) is invisible, so
 * it can be reported unused.
 * @param {Model[]} models @param {string} extraText
 * @returns {{ names: Set<string>, prefixes: string[] }}
 */
function gpMentions(models, extraText) {
  /** @type {Set<string>} */
  const names = new Set();
  /** @type {Set<string>} */
  const prefixes = new Set();
  /** @param {string} text */
  const scan = (text) => {
    for (const hit of text.matchAll(/(?<![\w-])gp-[a-z0-9][\w-]*/g)) {
      if (hit[0].endsWith("-")) prefixes.add(hit[0]);
      else names.add(hit[0]);
    }
  };
  for (const m of models) {
    if (m.kind === "css") {
      for (const cs of m.classSources) for (const t of cs.tokens) scan(t.util);
      continue;
    }
    scan(m.styleRanges.length === 0 ? m.clean : blank(m.clean, m.styleRanges));
    for (const cs of m.classSources) if (cs.tag < 0) for (const t of cs.tokens) scan(t.util);
  }
  scan(extraText);
  return { names, prefixes: [...prefixes] };
}

/** @param {string} name */
function isWidgetClass(name) {
  if (/^(?:btn|button|link|chip|pill|toggle|tab)$/.test(name)) return true;
  if (/^(?:btn|button|chip|pill|toggle|tab)-(?:primary|secondary|ghost|danger|success|remove|close|dismiss|action|x|icon)$/.test(name)) return true;
  if (/-(?:btn|button|link|chip|pill|toggle|action)$/.test(name)) return true;
  return /^close(?:-[\w-]+)?$/.test(name);
}

// ---------------------------------------------------------------------------
// Running the rules.
// ---------------------------------------------------------------------------

/** @param {string} src @returns {number[]} */
function lineStarts(src) {
  const starts = [0];
  for (let i = 0; i < src.length; i++) if (src[i] === "\n") starts.push(i + 1);
  return starts;
}

/** @param {number[]} starts @param {number} offset @returns {{ line: number, col: number }} */
function lineCol(starts, offset) {
  let lo = 0;
  let hi = starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (starts[mid] <= offset) lo = mid;
    else hi = mid - 1;
  }
  return { line: lo + 1, col: offset - starts[lo] + 1 };
}

/**
 * Lint one source text.
 * @param {string} file repository-relative path, slash form
 * @param {string} src
 * @param {Rule[]} [rules]
 * @returns {Violation[]}
 */
export function lintSource(file, src, rules = RULES) {
  return lintModel(buildModel(file, src), rules);
}

/**
 * The per-file rules over one built model.
 * @param {Model} m @param {Rule[]} rules @returns {Violation[]}
 */
function lintModel(m, rules) {
  const file = m.file;
  const starts = lineStarts(m.src);
  /** @type {Violation[]} */
  const out = [];
  for (const rule of rules) {
    if (!rule.run || !(m.kind === "svelte" ? rule.svelte : m.kind === "ts" ? rule.ts : rule.css)) continue;
    /** @type {Report} */
    const report = (offset, message, term) => {
      const { line, col } = lineCol(starts, offset);
      out.push(term === undefined ? { rule: rule.id, file, line, col, message } : { rule: rule.id, file, line, col, message, term });
    };
    rule.run(m, report);
  }
  return out.sort((a, b) => a.line - b.line || a.col - b.col || a.rule - b.rule);
}

/**
 * Directories never read at any depth: dependencies and version control. Build
 * output (dist, target, coverage reports) lives at the repository root, outside
 * src/, which is all the walk reads. It must not be listed by name here: a source
 * directory called `coverage` (src/lib/coverage) was skipped by that entry, so
 * none of its files was ever linted.
 */
const SKIP_DIRS = new Set(["node_modules", ".git", ".svelte-kit"]);
const NON_PRODUCTION = /(^|\/)__tests__\/|\.(?:test|spec)\.[cm]?[jt]s$|\.d\.ts$/;
const GENERATED_HEADER = /@generated|DO NOT EDIT|\.gen\.[jt]s$/i;

/**
 * Source files in scope, repository-relative, sorted.
 * @param {string} root
 * @returns {string[]}
 */
export function collectFiles(root) {
  /** @type {string[]} */
  const out = [];
  /** @param {string} dir */
  const walk = (dir) => {
    for (const entry of readdirSync(path.join(root, dir), { withFileTypes: true })) {
      const rel = dir === "" ? entry.name : `${dir}/${entry.name}`;
      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name)) walk(rel);
      } else if (/\.(?:svelte|ts|css)$/.test(entry.name) && !NON_PRODUCTION.test(rel)) {
        out.push(rel);
      }
    }
  };
  if (existsSync(path.join(root, "src"))) walk("src");
  return out.sort();
}

/**
 * @param {{ root?: string, rules?: Rule[] }} [opts]
 * @returns {{ violations: Violation[], files: number }}
 */
export function lintRepo({ root = REPO_ROOT, rules = RULES } = {}) {
  /** @type {[string, string][]} */
  const entries = [];
  for (const rel of collectFiles(root)) {
    const src = readFileSync(path.join(root, rel), "utf8");
    if (GENERATED_HEADER.test(src.split("\n", 5).join("\n")) || GENERATED_HEADER.test(rel)) continue;
    entries.push([rel, src]);
  }
  return lintFiles(entries, rules, readExtraText(root));
}

/**
 * Text outside src/ that may apply a gp-* class: the boot plate in index.html and the
 * scripts under public/. Rule 18 reads it so a class they use is not reported unused.
 * @param {string} root
 */
function readExtraText(root) {
  /** @type {string[]} */
  const parts = [];
  const index = path.join(root, "index.html");
  if (existsSync(index)) parts.push(readFileSync(index, "utf8"));
  const pub = path.join(root, "public");
  if (existsSync(pub)) {
    for (const entry of readdirSync(pub, { withFileTypes: true })) {
      if (entry.isFile() && /\.(?:js|mjs|html|css)$/.test(entry.name)) parts.push(readFileSync(path.join(pub, entry.name), "utf8"));
    }
  }
  return parts.join("\n");
}

/**
 * Lint a set of files held in memory: every per-file rule on each, then the rules
 * that need the whole tree (17 and 18). The one entry point both the disk walk and
 * the fixtures use.
 * @param {[string, string][]} entries repository-relative path (slash form) and text
 * @param {Rule[]} [rules] @param {string} [extraText]
 * @returns {{ violations: Violation[], files: number }}
 */
export function lintFiles(entries, rules = RULES, extraText = "") {
  const models = entries.map(([file, src]) => buildModel(file, src));
  /** @type {Violation[]} */
  const violations = models.flatMap((m) => lintModel(m, rules));
  /** @type {Map<string, number[]>} */
  const starts = new Map(models.map((m) => [m.file, lineStarts(m.src)]));
  for (const rule of rules) {
    const runRepo = rule.runRepo;
    if (!runRepo) continue;
    runRepo(
      models.filter((m) => (m.kind === "svelte" ? rule.svelte : m.kind === "ts" ? rule.ts : rule.css)),
      (file, offset, message, term) => {
        const { line, col } = lineCol(starts.get(file) ?? [0], offset);
        violations.push(term === undefined ? { rule: rule.id, file, line, col, message } : { rule: rule.id, file, line, col, message, term });
      },
      extraText,
    );
  }
  violations.sort((a, b) => (a.file < b.file ? -1 : a.file > b.file ? 1 : a.line - b.line || a.col - b.col || a.rule - b.rule));
  return { violations, files: models.length };
}

// ---------------------------------------------------------------------------
// Baseline and ratchet.
// ---------------------------------------------------------------------------

/**
 * @typedef {Record<string, Record<string, number>>} Counts rule id -> file -> count
 * @typedef {{ schema: 1, commit: string, retake: string, rules: Record<string, { name: string, total: number, files: Record<string, number> }> }} Baseline
 */

export const RETAKE_NOTE =
  "Taken at the commit above. It must be retaken at the overhaul BASE_SHA from a clean checkout (`node scripts/design-lint.mjs --baseline`), so another session's half-finished work never becomes the floor.";

/** @param {Violation[]} violations @returns {Counts} */
export function countViolations(violations) {
  /** @type {Counts} */
  const counts = {};
  for (const v of violations) {
    const byFile = (counts[String(v.rule)] ??= {});
    byFile[v.file] = (byFile[v.file] ?? 0) + 1;
  }
  return counts;
}

/**
 * @param {Violation[]} violations @param {string} commit @param {Rule[]} [rules]
 * @returns {Baseline}
 */
export function buildBaseline(violations, commit, rules = RULES) {
  const counts = countViolations(violations);
  /** @type {Baseline["rules"]} */
  const out = {};
  for (const rule of [...rules].sort((a, b) => a.id - b.id)) {
    const files = counts[String(rule.id)] ?? {};
    const sorted = Object.fromEntries(Object.entries(files).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
    out[String(rule.id)] = { name: rule.name, total: Object.values(sorted).reduce((a, b) => a + b, 0), files: sorted };
  }
  return { schema: 1, commit, retake: RETAKE_NOTE, rules: out };
}

/**
 * @param {Counts} current @param {Baseline} baseline
 * @returns {{ increases: { rule: number, file: string, before: number, after: number, isNew: boolean }[], decreases: { rule: number, file: string, before: number, after: number }[] }}
 */
export function compareToBaseline(current, baseline) {
  /** @type {{ rule: number, file: string, before: number, after: number, isNew: boolean }[]} */
  const increases = [];
  /** @type {{ rule: number, file: string, before: number, after: number }[]} */
  const decreases = [];
  const ids = new Set([...Object.keys(current), ...Object.keys(baseline.rules)]);
  for (const id of [...ids].sort((a, b) => Number(a) - Number(b))) {
    const now = current[id] ?? {};
    const was = baseline.rules[id]?.files ?? {};
    for (const file of [...new Set([...Object.keys(now), ...Object.keys(was)])].sort()) {
      const after = now[file] ?? 0;
      const before = was[file] ?? 0;
      if (after > before) increases.push({ rule: Number(id), file, before, after, isNew: before === 0 });
      else if (after < before) decreases.push({ rule: Number(id), file, before, after });
    }
  }
  return { increases, decreases };
}

/** @param {string} file @returns {Baseline} */
export function readBaseline(file) {
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  if (!parsed || parsed.schema !== 1 || typeof parsed.rules !== "object") throw new Error(`${file} is not a design-lint baseline (schema 1)`);
  return /** @type {Baseline} */ (parsed);
}

// ---------------------------------------------------------------------------
// Gate: allow-list, deferred files, strict verdict.
// ---------------------------------------------------------------------------

/**
 * @typedef {{ rule: number, file: string, reason: string, term?: string }} AllowEntry
 * @typedef {{ file: string, rules: number[], owner: string, reason: string }} DeferredEntry
 * @typedef {{ allow: AllowEntry[], deferred: DeferredEntry[] }} GateConfig
 */

export const DEFAULT_CONFIG_FILE = path.join(REPO_ROOT, "scripts", "design-lint.config.json");

/**
 * Read the gate config. A missing file is an empty config (nothing allowed,
 * nothing deferred); a malformed one throws, because a config that could not be
 * read must not quietly turn every exemption on or off.
 *
 * `allow` is permanent and every entry carries the reason it will never go
 * away (brand art with no token to map to; a vendor's own term). An entry may name a
 * `term` (rule 14), so it excuses that one word and not the rest of the rule. `deferred` is temporary: a file
 * another lane is rewriting, held to the ratchet alone until that lane merges.
 * The strict gate fails on an entry that matches no violation, so an exemption
 * cannot outlive the thing it excused.
 *
 * @param {string} file @param {Rule[]} [rules] @returns {GateConfig}
 */
export function readGateConfig(file, rules = RULES) {
  if (!existsSync(file)) return { allow: [], deferred: [] };
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(`${file} is not a design-lint config`);
  const ids = new Set(rules.map((r) => r.id));
  const nonEmpty = (/** @type {unknown} */ v) => typeof v === "string" && v.trim() !== "";
  /** @type {AllowEntry[]} */
  const allow = [];
  for (const [i, e] of (parsed.allow ?? []).entries()) {
    if (!ids.has(e?.rule) || !nonEmpty(e?.file) || !nonEmpty(e?.reason)) throw new Error(`${file}: allow[${i}] needs a rule (1-${rules.length}), a file and a non-empty reason`);
    if (e.term !== undefined && !nonEmpty(e.term)) throw new Error(`${file}: allow[${i}].term must be a non-empty string when present`);
    allow.push(e.term === undefined ? { rule: e.rule, file: e.file, reason: e.reason.trim() } : { rule: e.rule, file: e.file, reason: e.reason.trim(), term: e.term });
  }
  /** @type {DeferredEntry[]} */
  const deferred = [];
  for (const [i, e] of (parsed.deferred ?? []).entries()) {
    const ok = nonEmpty(e?.file) && nonEmpty(e?.owner) && nonEmpty(e?.reason) && Array.isArray(e?.rules) && e.rules.length > 0 && e.rules.every((/** @type {number} */ r) => ids.has(r));
    if (!ok) throw new Error(`${file}: deferred[${i}] needs a file, rules, an owner and a non-empty reason`);
    deferred.push({ file: e.file, rules: e.rules, owner: e.owner, reason: e.reason.trim() });
  }
  return { allow, deferred };
}

/**
 * Sort every violation into exactly one bucket: allowed (permanent, reported),
 * deferred (temporary, ratchet-only) or open (the gate fails on it). Entries that
 * matched nothing come back as `stale`.
 *
 * @param {Violation[]} violations @param {GateConfig} config
 * @returns {{ open: Violation[], allowed: (Violation & { reason: string })[], deferred: (Violation & { reason: string, owner: string })[], stale: string[] }}
 */
export function classifyViolations(violations, config) {
  /** @type {ReturnType<typeof classifyViolations>} */
  const out = { open: [], allowed: [], deferred: [], stale: [] };
  const used = new Set();
  for (const v of violations) {
    const a = config.allow.findIndex((e) => e.rule === v.rule && e.file === v.file && (e.term === undefined || e.term === v.term));
    if (a >= 0) {
      used.add(`a${a}`);
      out.allowed.push({ ...v, reason: config.allow[a].reason });
      continue;
    }
    const d = config.deferred.findIndex((e) => e.file === v.file && e.rules.includes(v.rule));
    if (d >= 0) {
      used.add(`d${d}`);
      out.deferred.push({ ...v, reason: config.deferred[d].reason, owner: config.deferred[d].owner });
      continue;
    }
    out.open.push(v);
  }
  config.allow.forEach((e, i) => {
    if (!used.has(`a${i}`)) out.stale.push(`allow: rule ${e.rule} ${e.file}${e.term === undefined ? "" : ` (${e.term})`} no longer has a violation; remove the entry`);
  });
  config.deferred.forEach((e, i) => {
    if (!used.has(`d${i}`)) out.stale.push(`deferred: ${e.file} (${e.owner}) no longer has a violation of rules ${e.rules.join(", ")}; remove the entry and retake the baseline`);
  });
  return out;
}

/** @param {{ file: string, rule: number }[]} hits @returns {string[]} one "rule N  file xCOUNT" line per pair */
function tally(hits) {
  /** @type {Map<string, number>} */
  const by = new Map();
  for (const h of hits) by.set(`rule ${String(h.rule).padStart(2)}  ${h.file}`, (by.get(`rule ${String(h.rule).padStart(2)}  ${h.file}`) ?? 0) + 1);
  return [...by.entries()].sort(([a], [b]) => (a < b ? -1 : 1)).map(([k, n]) => `${k} x${n}`);
}

/** @param {string} root @param {string[]} args @returns {string} */
function git(root, args) {
  return execFileSync("git", args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

// ---------------------------------------------------------------------------
// CLI.
// ---------------------------------------------------------------------------

export function usage() {
  return formatUsage({
    name: "design-lint",
    summary: "Check UI_RULES section 1 (rules 1-18) against a per-rule, per-file baseline that may only go down.",
    flags: [
      { flag: "--baseline", description: "write scripts/design-lint.baseline.json from the current tree and HEAD" },
      { flag: "--allow-dirty", description: "with --baseline: accept uncommitted changes under src/" },
      { flag: "--strict", description: "the gate: also fail on any violation not allow-listed or deferred in the config, and on a config entry that matches nothing" },
      { flag: "--config-file <file>", description: "alternate gate config path (default scripts/design-lint.config.json)" },
      { flag: "--json", description: "machine-readable result" },
      { flag: "--list", description: "print every violation, not only the ones over baseline" },
      { flag: "--rule <n>", description: "with --list: only this rule" },
      { flag: "--root <dir>", description: "repository root to scan" },
      { flag: "--baseline-file <file>", description: "alternate baseline path" },
      { flag: "--help, -h", description: "print this message and exit 0" },
    ],
    exits: "0 no count rose (with --strict: and nothing is open) · 1 a count rose, a new file has violations, or with --strict a violation is open or an exemption is stale · 2 the check could not run",
  });
}

/**
 * @param {string[]} argv
 */
export function parseArgs(argv) {
  const opts = { baseline: false, allowDirty: false, json: false, list: false, strict: false, rule: /** @type {number | null} */ (null), root: REPO_ROOT, baselineFile: "", configFile: "" };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    /** @returns {string} */
    const next = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`${arg} needs a value`);
      return v;
    };
    if (arg === "--baseline") opts.baseline = true;
    else if (arg === "--allow-dirty") opts.allowDirty = true;
    else if (arg === "--json") opts.json = true;
    else if (arg === "--list") opts.list = true;
    else if (arg === "--strict") opts.strict = true;
    else if (arg === "--config-file") opts.configFile = path.resolve(next());
    else if (arg === "--rule") {
      const n = Number(next());
      if (!Number.isInteger(n)) throw new Error("--rule needs a rule number");
      opts.rule = n;
    } else if (arg === "--root") opts.root = path.resolve(next());
    else if (arg === "--baseline-file") opts.baselineFile = path.resolve(next());
    else throw new Error(`unknown argument: ${arg}`);
  }
  if (opts.baselineFile === "") opts.baselineFile = opts.root === REPO_ROOT ? DEFAULT_BASELINE_FILE : path.join(opts.root, "scripts", "design-lint.baseline.json");
  if (opts.configFile === "") opts.configFile = opts.root === REPO_ROOT ? DEFAULT_CONFIG_FILE : path.join(opts.root, "scripts", "design-lint.config.json");
  return opts;
}

/** @param {Violation} v */
function formatViolation(v) {
  return `${v.file}:${v.line}:${v.col}  rule ${v.rule}  ${v.message}`;
}

/** @param {Counts} counts @param {Rule[]} rules @returns {Record<string, number>} */
function totalsOf(counts, rules) {
  /** @type {Record<string, number>} */
  const totals = {};
  for (const r of rules) totals[String(r.id)] = Object.values(counts[String(r.id)] ?? {}).reduce((a, b) => a + b, 0);
  return totals;
}

/**
 * @param {string[]} [argv]
 * @returns {number}
 */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  /** @type {ReturnType<typeof parseArgs>} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`design-lint: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
  try {
    const { violations, files } = lintRepo({ root: opts.root });
    const counts = countViolations(violations);
    const totals = totalsOf(counts, RULES);

    if (opts.baseline) {
      const commit = git(opts.root, ["rev-parse", "HEAD"]);
      const dirty = git(opts.root, ["status", "--porcelain", "--untracked-files=all", "--", "src"]);
      if (dirty !== "" && !opts.allowDirty) {
        console.error("design-lint: refusing to take a baseline over uncommitted changes under src/ (a baseline is taken at a commit); commit them or pass --allow-dirty");
        return 2;
      }
      mkdirSync(path.dirname(opts.baselineFile), { recursive: true });
      writeFileSync(opts.baselineFile, `${JSON.stringify(buildBaseline(violations, commit), null, 2)}\n`);
      console.log(`design-lint: baseline written to ${path.relative(opts.root, opts.baselineFile)} at ${commit.slice(0, 8)} (${files} files)`);
      for (const r of RULES) console.log(`  rule ${String(r.id).padStart(2)}  ${r.name.padEnd(22)} ${totals[String(r.id)]}`);
      return 0;
    }

    if (!existsSync(opts.baselineFile)) {
      console.error(`design-lint: no baseline at ${opts.baselineFile}; run with --baseline first (the check could not run)`);
      return 2;
    }
    const baseline = readBaseline(opts.baselineFile);
    const { increases, decreases } = compareToBaseline(counts, baseline);
    const config = readGateConfig(opts.configFile);
    const verdict = classifyViolations(violations, config);
    const strictOk = verdict.open.length === 0 && verdict.stale.length === 0;
    const ok = increases.length === 0 && (!opts.strict || strictOk);

    if (opts.json) {
      const over = new Set(increases.map((i) => `${i.rule}\0${i.file}`));
      console.log(
        JSON.stringify(
          {
            ok,
            strict: opts.strict,
            files,
            baselineCommit: baseline.commit,
            totals,
            baselineTotals: Object.fromEntries(Object.entries(baseline.rules).map(([id, r]) => [id, r.total])),
            increases,
            decreases,
            allowed: verdict.allowed,
            deferred: verdict.deferred,
            open: verdict.open,
            stale: verdict.stale,
            violations: violations.filter((v) => (opts.list ? opts.rule === null || v.rule === opts.rule : over.has(`${v.rule}\0${v.file}`))),
          },
          null,
          2,
        ),
      );
      return ok ? 0 : 1;
    }

    console.log(`design-lint: ${files} files, baseline at ${baseline.commit.slice(0, 8)}`);
    for (const r of RULES) {
      const now = totals[String(r.id)];
      const was = baseline.rules[String(r.id)]?.total;
      const delta = was === undefined ? " (not in baseline)" : now === was ? "" : ` (${now - was > 0 ? "+" : ""}${now - was} vs ${was})`;
      console.log(`  rule ${String(r.id).padStart(2)}  ${r.name.padEnd(22)} ${String(now).padStart(5)}${delta}`);
    }
    if (opts.list) {
      for (const v of violations) if (opts.rule === null || v.rule === opts.rule) console.log(formatViolation(v));
    }
    if (verdict.allowed.length > 0) {
      console.log(`
allowed (${verdict.allowed.length}, reported and never counted as failures):`);
      for (const line of tally(verdict.allowed)) console.log(`  ${line}`);
      for (const e of config.allow) console.log(`  why: ${e.file} rule ${e.rule}${e.term === undefined ? "" : ` (${e.term})`}: ${e.reason}`);
    }
    if (verdict.deferred.length > 0) {
      console.log(`
deferred (${verdict.deferred.length}, held to the ratchet until their owner merges):`);
      for (const line of tally(verdict.deferred)) console.log(`  ${line}`);
      /** @type {Map<string, number>} */
      const reasons = new Map();
      for (const e of config.deferred) reasons.set(`${e.owner}: ${e.reason}`, (reasons.get(`${e.owner}: ${e.reason}`) ?? 0) + 1);
      for (const [why, files] of reasons) console.log(`  why (${files} file${files === 1 ? "" : "s"}): ${why}`);
    }
    if (decreases.length > 0) {
      const gone = decreases.reduce((a, d) => a + d.before - d.after, 0);
      console.log(`\nprogress: ${gone} violation${gone === 1 ? "" : "s"} fixed across ${decreases.length} (rule, file) pair${decreases.length === 1 ? "" : "s"}; run --baseline to lock it in`);
    }
    if (!ok) {
      if (increases.length > 0) {
        console.log(`\nFAIL: ${increases.length} (rule, file) count${increases.length === 1 ? "" : "s"} rose over the baseline`);
        for (const inc of increases) {
          console.log(`  rule ${inc.rule}  ${inc.file}: ${inc.before} -> ${inc.after}${inc.isNew ? " (new)" : ""}`);
          for (const v of violations) if (v.rule === inc.rule && v.file === inc.file) console.log(`    ${formatViolation(v)}`);
        }
      }
      if (opts.strict && verdict.open.length > 0) {
        console.log(`\nFAIL: ${verdict.open.length} violation${verdict.open.length === 1 ? "" : "s"} outside the allow-list (fix them; the allow-list is for brand art with a reason, not for debt)`);
        for (const v of verdict.open) console.log(`  ${formatViolation(v)}`);
      }
      if (opts.strict && verdict.stale.length > 0) {
        console.log(`\nFAIL: ${verdict.stale.length} stale config entr${verdict.stale.length === 1 ? "y" : "ies"}`);
        for (const line of verdict.stale) console.log(`  ${line}`);
      }
      return 1;
    }
    console.log(opts.strict ? "\nOK: no count rose over the baseline and nothing is open outside the allow-list" : "\nOK: no count rose over the baseline");
    return 0;
  } catch (err) {
    console.error(`design-lint: internal error: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
}

if (process.argv[1] && statSafe(process.argv[1]) && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}

/** @param {string} p */
function statSafe(p) {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}
