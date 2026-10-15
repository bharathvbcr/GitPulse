import { readdirSync } from "node:fs";
import { join } from "node:path";

/** Shared by the source-scanning contract tests; keep one copy of each helper. */

/** Every file under `dir` whose name `accept`s. */
export function filesUnder(dir: string, accept: (name: string) => boolean): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return filesUnder(path, accept);
    return accept(entry.name) ? [path] : [];
  });
}

export function svelteFiles(dir: string): string[] {
  return filesUnder(dir, (name) => name.endsWith(".svelte"));
}

/** Non-test TypeScript sources under `dir`. */
export function tsSourceFiles(dir: string): string[] {
  return filesUnder(dir, (name) => name.endsWith(".ts") && !name.endsWith(".test.ts"));
}

/**
 * The script with comments and string contents blanked to spaces, so neither
 * a comment that names `$repoStore.x` nor a `)` inside a string is read as
 * code. Template-literal `${...}` expressions are code and are kept. Offsets
 * are preserved, so a match maps back to its line.
 */
export function codeOnly(script: string): string {
  const out = script.split("");
  const blank = (from: number, to: number) => {
    for (let i = from; i < to; i += 1) if (out[i] !== "\n") out[i] = " ";
  };
  // Braces opened inside template expressions, innermost last; a `}` that
  // closes one of these resumes the template literal around it.
  const templateDepths: number[] = [];
  let depth = 0;
  let lastSignificant = "";
  let i = 0;
  const scanTemplate = (start: number): number => {
    let j = start;
    while (j < script.length) {
      if (script[j] === "\\") { j += 2; continue; }
      if (script[j] === "`") { blank(start, j); return j + 1; }
      if (script[j] === "$" && script[j + 1] === "{") {
        blank(start, j);
        templateDepths.push(depth);
        depth += 1;
        return j + 2;
      }
      j += 1;
    }
    blank(start, j);
    return j;
  };
  while (i < script.length) {
    const c = script[i];
    const next = script[i + 1];
    if (c === "/" && next === "/") {
      const end = script.indexOf("\n", i);
      const stop = end === -1 ? script.length : end;
      blank(i, stop);
      i = stop;
      continue;
    }
    if (c === "/" && next === "*") {
      const end = script.indexOf("*/", i + 2);
      const stop = end === -1 ? script.length : end + 2;
      blank(i, stop);
      i = stop;
      continue;
    }
    if (c === '"' || c === "'") {
      let j = i + 1;
      while (j < script.length && script[j] !== c && script[j] !== "\n") j += script[j] === "\\" ? 2 : 1;
      blank(i + 1, j);
      i = j + 1;
      lastSignificant = c;
      continue;
    }
    if (c === "`") {
      i = scanTemplate(i + 1);
      lastSignificant = "`";
      continue;
    }
    if (c === "/" && (lastSignificant === "" || "(,=:[!&|?{};+-*%<>~^".includes(lastSignificant))) {
      // A regular expression literal: skip to its closing slash.
      let j = i + 1;
      let inClass = false;
      while (j < script.length && script[j] !== "\n") {
        if (script[j] === "\\") { j += 2; continue; }
        if (script[j] === "[") inClass = true;
        else if (script[j] === "]") inClass = false;
        else if (script[j] === "/" && !inClass) break;
        j += 1;
      }
      blank(i + 1, j);
      i = j + 1;
      lastSignificant = "/";
      continue;
    }
    if (c === "{") depth += 1;
    if (c === "}") {
      depth -= 1;
      if (templateDepths.length > 0 && templateDepths[templateDepths.length - 1] === depth) {
        templateDepths.pop();
        i = scanTemplate(i + 1);
        lastSignificant = "`";
        continue;
      }
    }
    if (!/\s/.test(c)) lastSignificant = /[\w$]/.test(c) ? "a" : c;
    i += 1;
  }
  return out.join("");
}

/** The text of one top-level script function, up to the next one (or the end). */
export function functionBody(source: string, name: string): string {
  const start = source.search(new RegExp(`function ${name}\\(`));
  if (start === -1) throw new Error(`no function ${name}`);
  const rest = source.slice(start + 1);
  const next = rest.search(/\n  (?:async )?function \w+\(|\n<\/script>/);
  return next === -1 ? rest : rest.slice(0, next);
}
