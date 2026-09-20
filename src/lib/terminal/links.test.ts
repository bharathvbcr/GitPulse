import { describe, expect, it } from "vitest";
import {
  detectTerminalLinks,
  isOpenableUrl,
  resolveLinkAction,
  linksForRow,
  logicalLineAt,
  parseFileTarget,
  resolveRepoFile,
  scanWasTruncated,
  MAX_LINKS_PER_LINE,
  MAX_PATH_LENGTH,
  MAX_SCAN_LENGTH,
  MAX_URL_LENGTH,
  MAX_WRAP_ROWS,
  OPENABLE_URL_SCHEMES,
  type LinkBuffer,
} from "./links";

/** A buffer of literal rows. `widths` gives per-character cell counts, so a
 * test can place double-width characters where a real CJK line would. */
function makeBuffer(rows: Array<{ text: string; isWrapped?: boolean; widths?: number[] }>): LinkBuffer {
  return {
    row(index: number) {
      const source = rows[index - 1];
      if (!source) return null;
      const columns: number[] = [];
      let column = 1;
      for (let i = 0; i < source.text.length; i++) {
        columns.push(column);
        column += source.widths?.[i] ?? 1;
      }
      return { isWrapped: !!source.isWrapped, text: source.text, columns };
    },
  };
}

const urls = (line: string) => detectTerminalLinks(line).filter(l => l.kind === "url").map(l => l.text);
const files = (line: string) => detectTerminalLinks(line).filter(l => l.kind === "file").map(l => l.text);

describe("isOpenableUrl", () => {
  it("accepts the two web schemes, in any case", () => {
    expect(isOpenableUrl("http://example.com")).toBe(true);
    expect(isOpenableUrl("https://example.com/a/b?c=d#e")).toBe(true);
    expect(isOpenableUrl("HTTPS://EXAMPLE.COM")).toBe(true);
  });

  it("refuses every other scheme, including ones a handler is installed for", () => {
    for (const url of [
      "javascript://example.com/%0aalert(1)",
      "data:text/html,<script>alert(1)</script>",
      "file:///etc/passwd",
      "vscode://file/etc/passwd",
      "smb://attacker/share",
      "ftp://example.com/x",
      "ssh://example.com",
      "chrome://settings",
      "x-gitpulse://open",
    ]) {
      expect(isOpenableUrl(url), url).toBe(false);
    }
  });

  it("refuses what will not parse, is empty, or carries control characters", () => {
    expect(isOpenableUrl("")).toBe(false);
    expect(isOpenableUrl("http://")).toBe(false);
    expect(isOpenableUrl("not a url")).toBe(false);
    expect(isOpenableUrl("https://exa mple.com")).toBe(false);
    expect(isOpenableUrl(`https://example.com/${String.fromCharCode(10)}x`)).toBe(false);
    expect(isOpenableUrl(`https://example.com/${String.fromCharCode(0)}`)).toBe(false);
  });

  it("refuses a URL past the length cap", () => {
    const long = `https://example.com/${"a".repeat(MAX_URL_LENGTH)}`;
    expect(long.length).toBeGreaterThan(MAX_URL_LENGTH);
    expect(isOpenableUrl(long)).toBe(false);
  });

  it("names exactly the schemes the module documents", () => {
    expect([...OPENABLE_URL_SCHEMES].sort()).toEqual(["http", "https"]);
  });
});

describe("detectTerminalLinks — URLs", () => {
  it("finds a bare URL and reports its exact span", () => {
    const line = "see https://example.com/a now";
    const [link] = detectTerminalLinks(line);
    expect(link).toMatchObject({ kind: "url", text: "https://example.com/a" });
    expect(line.slice(link.start, link.end)).toBe(link.text);
  });

  it("drops sentence punctuation but keeps brackets the URL opened", () => {
    expect(urls("go to https://example.com/a.")).toEqual(["https://example.com/a"]);
    expect(urls("(see https://example.com)")).toEqual(["https://example.com"]);
    expect(urls("https://en.wikipedia.org/wiki/Shell_(computing)")).toEqual([
      "https://en.wikipedia.org/wiki/Shell_(computing)",
    ]);
    expect(urls("ref: https://example.com/a?b=1, and more")).toEqual(["https://example.com/a?b=1"]);
  });

  it("finds several URLs on one line without overlapping them", () => {
    const line = "a https://one.example b http://two.example c";
    expect(urls(line)).toEqual(["https://one.example", "http://two.example"]);
  });

  it("leaves a refused scheme entirely untouched — no link of either kind", () => {
    for (const line of ["run file:///etc/passwd", "open vscode://file/x/y.ts", "eval javascript://x/%0aalert(1)"]) {
      expect(detectTerminalLinks(line), line).toEqual([]);
    }
  });

  it("does not claim a second URL inside the first", () => {
    const found = detectTerminalLinks("https://a.example/http://b.example");
    expect(found).toHaveLength(1);
    expect(found[0].text).toBe("https://a.example/http://b.example");
  });

  it("ignores a scheme-shaped span with no scheme letter", () => {
    expect(detectTerminalLinks("://example.com")).toEqual([]);
    expect(detectTerminalLinks("1://example.com")).toEqual([]);
  });
});

describe("parseFileTarget", () => {
  it("reads path, line and column in the conventional order", () => {
    expect(parseFileTarget("src/lib/a.ts:12:5")).toEqual({ path: "src/lib/a.ts", line: 12, column: 5 });
    expect(parseFileTarget("src/lib/a.ts:12")).toEqual({ path: "src/lib/a.ts", line: 12, column: null });
    expect(parseFileTarget("src/lib/a.ts")).toEqual({ path: "src/lib/a.ts", line: null, column: null });
  });

  it("accepts the separator styles real output prints", () => {
    expect(parseFileTarget("./src/a.ts")?.path).toBe("./src/a.ts");
    expect(parseFileTarget("../sibling/b.rs:3")?.path).toBe("../sibling/b.rs");
    expect(parseFileTarget("src\\lib\\a.ts:9")).toEqual({ path: "src\\lib\\a.ts", line: 9, column: null });
    expect(parseFileTarget("/abs/path/c.rs:40")).toEqual({ path: "/abs/path/c.rs", line: 40, column: null });
  });

  it("accepts a bare filename only when a line number makes the intent explicit", () => {
    expect(parseFileTarget("main.rs:42")).toEqual({ path: "main.rs", line: 42, column: null });
    expect(parseFileTarget("main.rs")).toBeNull();
    expect(parseFileTarget("cargo")).toBeNull();
    expect(parseFileTarget("build")).toBeNull();
  });

  it("does not read a Windows drive letter as a line number", () => {
    expect(parseFileTarget("C:\\src\\a.ts:7")).toEqual({ path: "C:\\src\\a.ts", line: 7, column: null });
    expect(parseFileTarget("C:")).toBeNull();
  });

  it("refuses a URI scheme even when it has no `//` to give it away", () => {
    // These read as relative paths to a naive check because they contain a
    // `/`, which is how a refused scheme would otherwise be laundered into a
    // repository file reference.
    expect(parseFileTarget("data:text/html,<script>")).toBeNull();
    expect(parseFileTarget("mailto:a@b.example/x")).toBeNull();
    expect(parseFileTarget("about:blank/x")).toBeNull();
    // A Windows drive is one letter and stays a path.
    expect(parseFileTarget("C:/src/a.ts")?.path).toBe("C:/src/a.ts");
  });

  it("refuses URLs, protocol-relative references, and separators with no name", () => {
    expect(parseFileTarget("https://example.com/a.ts:1")).toBeNull();
    expect(parseFileTarget("//example.com/a.ts")).toBeNull();
    expect(parseFileTarget("/")).toBeNull();
    expect(parseFileTarget("//")).toBeNull();
    expect(parseFileTarget("../..")).toBeNull();
    expect(parseFileTarget("...")).toBeNull();
  });

  it("refuses a NUL byte and an over-long path", () => {
    expect(parseFileTarget(`src/a${String.fromCharCode(0)}.ts`)).toBeNull();
    expect(parseFileTarget(`src/${"a".repeat(MAX_PATH_LENGTH)}.ts`)).toBeNull();
  });

  it("refuses an absurd line number rather than coercing it", () => {
    expect(parseFileTarget("a.ts:99999999999999")).toBeNull();
    expect(parseFileTarget("src/a.ts:0")).toEqual({ path: "src/a.ts:0", line: null, column: null });
  });
});

describe("detectTerminalLinks — files", () => {
  it("finds paths in the shapes build tools print", () => {
    expect(files("error in src/lib/a.ts:12:5 — bad")).toEqual(["src/lib/a.ts:12:5"]);
    expect(files("  --> src/main.rs:40:9")).toEqual(["src/main.rs:40:9"]);
    expect(files("modified:   src/lib/terminal/links.ts")).toEqual(["src/lib/terminal/links.ts"]);
  });

  it("strips the punctuation output wraps a path in", () => {
    expect(files("at (src/a.ts:3)")).toEqual(["src/a.ts:3"]);
    expect(files('see "src/b.ts" for more')).toEqual(["src/b.ts"]);
    expect(files("[src/c.ts:9]")).toEqual(["src/c.ts:9"]);
    expect(files("failed: src/d.ts.")).toEqual(["src/d.ts"]);
    expect(files("--out=dist/bundle.js")).toEqual(["dist/bundle.js"]);
  });

  it("never claims a span a URL already owns", () => {
    const found = detectTerminalLinks("https://example.com/src/a.ts:12 and src/b.ts:3");
    expect(found.map(l => [l.kind, l.text])).toEqual([
      ["url", "https://example.com/src/a.ts:12"],
      ["file", "src/b.ts:3"],
    ]);
  });

  it("leaves ordinary prose alone", () => {
    expect(detectTerminalLinks("running 42 tests, all passed in 1.2s")).toEqual([]);
    expect(detectTerminalLinks("Compiling gitpulse v1.3.0")).toEqual([]);
  });
});

describe("detectTerminalLinks — invariants", () => {
  const corpus = [
    "https://a.example src/a.ts:1 https://b.example/x) [src/b.ts]",
    "(https://c.example/(x)) ../a/b.rs:2:3 file:///etc/passwd",
    "a".repeat(300),
    "://:// ::: /// \\\\\\ ...",
    `mix ${"https://x.example/a ".repeat(40)}`,
  ];

  it("returns ordered, non-overlapping spans whose text matches the source", () => {
    for (const line of corpus) {
      const found = detectTerminalLinks(line);
      let previousEnd = -1;
      for (const link of found) {
        expect(link.start, line).toBeGreaterThanOrEqual(previousEnd);
        expect(link.end, line).toBeGreaterThan(link.start);
        expect(line.slice(link.start, link.end), line).toBe(link.text);
        previousEnd = link.end;
      }
    }
  });

  it("never reports more links than the cap", () => {
    const line = `${"https://x.example ".repeat(MAX_LINKS_PER_LINE * 3)}`;
    expect(detectTerminalLinks(line).length).toBeLessThanOrEqual(MAX_LINKS_PER_LINE);
  });

  it("caps the scan and says so rather than reporting a short read as a clean one", () => {
    const tail = "https://tail.example";
    const line = `${"x".repeat(MAX_SCAN_LENGTH)} ${tail}`;
    expect(scanWasTruncated(line)).toBe(true);
    expect(urls(line)).not.toContain(tail);
    expect(scanWasTruncated("short line")).toBe(false);
  });
});

describe("detectTerminalLinks — adversarial input", () => {
  /**
   * These would each be a freeze rather than a failure if any pass
   * backtracked or re-scanned. The budget is deliberately loose: it separates
   * linear from quadratic-or-worse without turning into a load-sensitive
   * flake on a busy machine.
   */
  const budgetMs = 2000;
  const timed = (line: string) => {
    const started = performance.now();
    const found = detectTerminalLinks(line);
    return { elapsed: performance.now() - started, found };
  };

  it("survives a line that is nothing but closing brackets after a URL", () => {
    const { elapsed } = timed(`https://example.com/${")".repeat(MAX_SCAN_LENGTH)}`);
    expect(elapsed).toBeLessThan(budgetMs);
  });

  it("survives a line of scheme separators", () => {
    const { elapsed, found } = timed("://".repeat(MAX_SCAN_LENGTH / 3));
    expect(found).toEqual([]);
    expect(elapsed).toBeLessThan(budgetMs);
  });

  it("survives deeply nested relative segments and long colon runs", () => {
    expect(timed(`${"../".repeat(2000)}a.ts:1`).elapsed).toBeLessThan(budgetMs);
    expect(timed(`a.ts${":1".repeat(2000)}`).elapsed).toBeLessThan(budgetMs);
    expect(timed(`${"a/".repeat(4000)}b.ts`).elapsed).toBeLessThan(budgetMs);
  });

  it("survives a maximal line of mixed candidates", () => {
    const unit = "https://x.example/a) src/a.ts:1:2 file:///x (b/c.rs:3) ";
    const line = unit.repeat(Math.ceil(MAX_SCAN_LENGTH / unit.length));
    const { elapsed, found } = timed(line);
    expect(elapsed).toBeLessThan(budgetMs);
    expect(found.length).toBeLessThanOrEqual(MAX_LINKS_PER_LINE);
  });
});

describe("resolveRepoFile", () => {
  const root = "/Users/x/Code/GitPulse";

  it("resolves a relative path to a repository-relative POSIX path", () => {
    expect(resolveRepoFile(root, { path: "src/lib/a.ts", line: 3, column: null }))
      .toEqual({ path: "src/lib/a.ts", line: 3, column: null });
    expect(resolveRepoFile(root, { path: "./src/a.ts", line: null, column: null })?.path).toBe("src/a.ts");
    expect(resolveRepoFile(root, { path: "src\\lib\\a.ts", line: null, column: null })?.path).toBe("src/lib/a.ts");
    expect(resolveRepoFile(root, { path: "src/./lib/../a.ts", line: null, column: null })?.path).toBe("src/a.ts");
  });

  it("resolves an absolute path that is genuinely inside the repository", () => {
    expect(resolveRepoFile(root, { path: `${root}/src/a.ts`, line: 9, column: 2 }))
      .toEqual({ path: "src/a.ts", line: 9, column: 2 });
  });

  it("refuses a relative path that climbs out", () => {
    expect(resolveRepoFile(root, { path: "../secrets.txt", line: null, column: null })).toBeNull();
    expect(resolveRepoFile(root, { path: "src/../../secrets.txt", line: null, column: null })).toBeNull();
  });

  it("refuses an absolute path outside, including a sibling that shares the prefix", () => {
    expect(resolveRepoFile(root, { path: "/etc/passwd", line: null, column: null })).toBeNull();
    // The string-prefix bug this guards: `/Users/x/Code/GitPulse-evil` starts
    // with the root but is a different directory.
    expect(resolveRepoFile(root, { path: `${root}-evil/src/a.ts`, line: null, column: null })).toBeNull();
    expect(resolveRepoFile(root, { path: `${root}/../other/a.ts`, line: null, column: null })).toBeNull();
  });

  it("refuses the repository root itself — it is not a file", () => {
    expect(resolveRepoFile(root, { path: root, line: null, column: null })).toBeNull();
  });

  it("refuses a path on another Windows drive", () => {
    expect(resolveRepoFile("C:/Code/GitPulse", { path: "D:/secrets.txt", line: null, column: null })).toBeNull();
    expect(resolveRepoFile("C:/Code/GitPulse", { path: "C:/Code/GitPulse/src/a.ts", line: null, column: null })?.path)
      .toBe("src/a.ts");
  });

  it("refuses when there is no repository to resolve against", () => {
    expect(resolveRepoFile("", { path: "src/a.ts", line: null, column: null })).toBeNull();
  });

  it("round-trips every file span the detector produces without escaping the repo", () => {
    const line = "see ../../../etc/passwd:1 and src/ok.ts:2 and /etc/shadow:3";
    const resolved = detectTerminalLinks(line)
      .filter(l => l.kind === "file")
      .map(l => {
        const target = parseFileTarget(l.text);
        return target ? resolveRepoFile(root, target) : null;
      });
    expect(resolved.filter(Boolean).map(r => r?.path)).toEqual(["src/ok.ts"]);
  });
});

describe("detectTerminalLinks — fuzz", () => {
  /** Deterministic so a failure is reproducible; a seeded run that flakes is
   * not a stress test, it is a coin toss with extra steps. */
  function rng(seed: number) {
    let state = seed >>> 0;
    return () => {
      state = (state * 1664525 + 1013904223) >>> 0;
      return state / 0x100000000;
    };
  }

  /** Fragments chosen to collide with every rule in the module: scheme
   * prefixes without authorities, unbalanced brackets, separators with no
   * name, drive letters, colon runs, wide characters and control codes. */
  const FRAGMENTS = [
    "https://", "http://", "://", "file://", "javascript:", "data:text/html,", "vscode://",
    "a", "z9", "src", "lib", "..", ".", "/", "\\", ":", "::", ":1", ":12:5", "?x=1", "&y=2",
    "(", ")", "[", "]", "{", "}", '"', "'", "`", "<", ">", "|", ",", ";", "!", "*", "=",
    ".ts", ".rs", ".example", "C:", "C:/", "//", " ", "\t", "世界", "🚀",
    String.fromCharCode(0), String.fromCharCode(27), String.fromCharCode(10), String.fromCharCode(160),
    "example.com", "etc/passwd", "-".repeat(40), "9".repeat(20),
  ];

  const root = "/Users/x/Code/GitPulse";

  it("holds every invariant across 20,000 generated lines", () => {
    const next = rng(0x5eed1e);
    let withLinks = 0;
    for (let iteration = 0; iteration < 20_000; iteration++) {
      const parts: string[] = [];
      const pieces = 1 + Math.floor(next() * 12);
      for (let i = 0; i < pieces; i++) {
        parts.push(FRAGMENTS[Math.floor(next() * FRAGMENTS.length)]);
      }
      const line = parts.join("");
      const found = detectTerminalLinks(line);
      if (found.length) withLinks += 1;

      let previousEnd = 0;
      for (const link of found) {
        // In bounds, ordered, non-overlapping, and quoting the source exactly.
        expect(link.start, line).toBeGreaterThanOrEqual(previousEnd);
        expect(link.end, line).toBeGreaterThan(link.start);
        expect(link.end, line).toBeLessThanOrEqual(line.length);
        expect(line.slice(link.start, link.end), line).toBe(link.text);
        previousEnd = link.end;

        // The two policy guarantees, asserted on generated input rather than
        // on the examples the rules were written against.
        const action = resolveLinkAction(link.text, root);
        if (action.kind === "url") {
          expect(new URL(action.url).protocol, line).toMatch(/^https?:$/);
        }
        if (action.kind === "file") {
          expect(action.path.startsWith("/"), line).toBe(false);
          expect(action.path.split("/"), line).not.toContain("..");
          expect(action.path, line).not.toContain("\0");
        }
      }
      expect(found.length, line).toBeLessThanOrEqual(MAX_LINKS_PER_LINE);
    }
    // A fuzz run that never produced a link would satisfy every assertion
    // above while testing nothing at all.
    expect(withLinks).toBeGreaterThan(200);
  });

  it("refuses exactly the generated paths that leave the repository", () => {
    const next = rng(0xc0ffee);
    let refusals = 0;
    let accepted = 0;
    for (let iteration = 0; iteration < 20_000; iteration++) {
      const depth = Math.floor(next() * 6);
      const absolute = next() < 0.5;
      const inside = next() < 0.5;
      const prefix = absolute ? root : "";
      const path = `${prefix}/${"../".repeat(depth)}${inside ? "src" : "etc"}/passwd${next() < 0.5 ? ":1" : ""}`
        .replace(/^\//, absolute ? "/" : "");
      const target = parseFileTarget(path);
      const resolved = target ? resolveRepoFile(root, target) : null;

      /**
       * An oracle, not a shape check.
       *
       * Asserting only that an *accepted* result looks relative and holds no
       * `..` is satisfied by a bug that resolves the climb away instead of
       * refusing it: `../etc/passwd` becomes `etc/passwd`, which passes every
       * such check while pointing somewhere the caller never sanctioned. The
       * question has to be whether this input should have been refused at
       * all, computed independently of the code under test.
       */
      const rootDepth = root.replace(/^\//, "").split("/").length;
      const escapes = absolute ? depth > rootDepth : depth > 0;
      if (escapes) {
        expect(resolved, path).toBeNull();
        refusals += 1;
      } else if (resolved) {
        expect(resolved.path.split("/"), path).not.toContain("..");
        expect(resolved.path.startsWith("/"), path).toBe(false);
        accepted += 1;
      }
    }
    // Both branches must actually have been taken.
    expect(refusals).toBeGreaterThan(1000);
    expect(accepted).toBeGreaterThan(1000);
  });
});

describe("resolveLinkAction", () => {
  const root = "/Users/x/Code/GitPulse";

  it("opens an http or https target", () => {
    expect(resolveLinkAction("https://example.com/a", root)).toEqual({ kind: "url", url: "https://example.com/a" });
    expect(resolveLinkAction("  http://example.com  ", root)).toEqual({ kind: "url", url: "http://example.com" });
  });

  it("opens a file inside the repository, carrying the line and column", () => {
    expect(resolveLinkAction("src/lib/a.ts:12:5", root))
      .toEqual({ kind: "file", path: "src/lib/a.ts", line: 12, column: 5 });
  });

  /**
   * An OSC 8 hyperlink sets its display text and its target independently, so
   * this is the path where the text on screen proves nothing about what a
   * click does. Every refusal below is a target that never reaches an opener.
   */
  it("refuses an OSC 8 target whose scheme is not http or https", () => {
    for (const target of [
      "file:///etc/passwd",
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vscode://file/etc/passwd",
      "smb://attacker/share",
    ]) {
      expect(resolveLinkAction(target, root), target).toMatchObject({ kind: "refused" });
    }
  });

  it("refuses a file target that escapes the repository, and says why", () => {
    const action = resolveLinkAction("../../../etc/passwd:1", root);
    expect(action.kind).toBe("refused");
    expect(action.kind === "refused" && action.reason).toContain("outside this repository");
    expect(resolveLinkAction("/etc/shadow:1", root).kind).toBe("refused");
  });

  it("refuses an empty or unrecognisable target rather than guessing", () => {
    expect(resolveLinkAction("", root)).toMatchObject({ kind: "refused" });
    expect(resolveLinkAction("   ", root)).toMatchObject({ kind: "refused" });
    expect(resolveLinkAction("just some words", root)).toMatchObject({ kind: "refused" });
  });

  it("refuses everything when there is no repository bound to the session", () => {
    expect(resolveLinkAction("src/a.ts:1", "")).toMatchObject({ kind: "refused" });
    // A URL still works: it does not resolve against the repository.
    expect(resolveLinkAction("https://example.com", "")).toMatchObject({ kind: "url" });
  });

  it("never returns an openable action for any span the detector refused", () => {
    const line = "file:///etc/passwd javascript://x/%0aalert(1) vscode://file/x ../../etc/passwd";
    for (const token of line.split(" ")) {
      const action = resolveLinkAction(token, root);
      expect(action.kind, token).toBe("refused");
    }
  });
});

describe("logicalLineAt / linksForRow", () => {
  it("places a link on the row and columns it actually occupies", () => {
    const buffer = makeBuffer([{ text: "see https://example.com/a here" }]);
    const [link] = linksForRow(buffer, 1);
    expect(link.text).toBe("https://example.com/a");
    expect(link.range).toEqual({ start: { x: 5, y: 1 }, end: { x: 25, y: 1 } });
  });

  it("joins a wrapped line and spans the link across both rows", () => {
    const buffer = makeBuffer([
      { text: "visit https://example" },
      { text: ".com/deep/path ok", isWrapped: true },
    ]);
    const [link] = linksForRow(buffer, 1);
    expect(link.text).toBe("https://example.com/deep/path");
    expect(link.range.start).toEqual({ x: 7, y: 1 });
    expect(link.range.end).toEqual({ x: 14, y: 2 });
  });

  it("finds the same link when the pointer is on the continuation row", () => {
    const buffer = makeBuffer([
      { text: "visit https://example" },
      { text: ".com/deep/path ok", isWrapped: true },
    ]);
    expect(linksForRow(buffer, 2)).toEqual(linksForRow(buffer, 1));
  });

  it("does not absorb the row below when that row starts its own line", () => {
    const buffer = makeBuffer([
      { text: "https://one.example" },
      { text: "https://two.example", isWrapped: false },
    ]);
    expect(linksForRow(buffer, 1).map(l => l.text)).toEqual(["https://one.example"]);
    expect(linksForRow(buffer, 2).map(l => l.text)).toEqual(["https://two.example"]);
  });

  it("counts a double-width character as two cells, not one", () => {
    // Two CJK glyphs then a space, so the URL starts at string offset 3 but
    // at column 6. Reading the offset as the column underlines the wrong text.
    const buffer = makeBuffer([{ text: "世界 https://x.example", widths: [2, 2, 1] }]);
    const [link] = linksForRow(buffer, 1);
    expect(link.text).toBe("https://x.example");
    expect(link.range.start).toEqual({ x: 6, y: 1 });
    expect(link.range.end.x).toBe(6 + "https://x.example".length - 1);
  });

  it("bounds the climb and the descent, and says when it stopped early", () => {
    const rows = Array.from({ length: MAX_WRAP_ROWS * 3 }, (_, i) => ({ text: "x".repeat(40), isWrapped: i > 0 }));
    const buffer = makeBuffer(rows);
    const fromTop = logicalLineAt(buffer, 1);
    expect(fromTop.truncated).toBe(true);
    expect(fromTop.text.length).toBeLessThanOrEqual(MAX_SCAN_LENGTH);
    const fromDeep = logicalLineAt(buffer, MAX_WRAP_ROWS * 2);
    expect(fromDeep.truncated).toBe(true);
    expect(fromDeep.cells.length).toBe(fromDeep.text.length);
  });

  it("returns nothing for an empty or missing row instead of throwing", () => {
    const buffer = makeBuffer([{ text: "" }]);
    expect(linksForRow(buffer, 1)).toEqual([]);
    expect(linksForRow(buffer, 99)).toEqual([]);
    expect(logicalLineAt(buffer, 99)).toEqual({ text: "", cells: [], truncated: false });
  });

  it("keeps one cell per character across a wrap", () => {
    const buffer = makeBuffer([
      { text: "abc" },
      { text: "def", isWrapped: true },
    ]);
    const line = logicalLineAt(buffer, 1);
    expect(line.text).toBe("abcdef");
    expect(line.cells).toEqual([
      { x: 1, y: 1 }, { x: 2, y: 1 }, { x: 3, y: 1 },
      { x: 1, y: 2 }, { x: 2, y: 2 }, { x: 3, y: 2 },
    ]);
  });
});
