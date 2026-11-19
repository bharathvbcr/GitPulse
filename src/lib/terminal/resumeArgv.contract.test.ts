import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { stripMarkupComments } from "../dom/markupText";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(HERE, "..", "..", "..");

function walkFiles(dir: string, exts: readonly string[], skipDir?: (full: string) => boolean): string[] {
  const results: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      if (skipDir?.(full)) continue;
      results.push(...walkFiles(full, exts, skipDir));
    } else if (exts.some((ext) => entry.endsWith(ext))) {
      results.push(full);
    }
  }
  return results;
}

function stripBlockComments(source: string): string {
  let out = source;
  for (;;) {
    const start = out.indexOf("/*");
    if (start === -1) return out;
    const end = out.indexOf("*/", start + 2);
    if (end === -1) return out.slice(0, start);
    out = out.slice(0, start) + out.slice(end + 2);
  }
}

function stripJsAndSvelteComments(source: string): string {
  let current = source;
  for (let prev = ""; prev !== current; ) {
    prev = current;
    current = stripBlockComments(stripMarkupComments(current));
  }
  return current.replace(/(^|[^:])\/\/.*$/gm, "$1");
}

function stripRustNonTestCode(source: string): string {
  // Production files place their test module under `#[cfg(test)] mod tests`.
  const cutPatterns = [/\n#\[cfg\(test\)\]\s*\n\s*(?:pub(?:\(crate\))?\s+)?mod\s+tests\b/];
  let head = source;
  for (const pat of cutPatterns) {
    const m = pat.exec(head);
    if (m) head = head.slice(0, m.index);
  }
  for (let prev = ""; prev !== head; ) {
    prev = head;
    head = stripBlockComments(head);
  }
  return head.replace(/\/\/.*$/gm, "");
}

describe("S2-07 single resume-argv owner contract", () => {
  it("constructs zero --resume / --conversation / --session-id / --continue / --fork-session argv flags anywhere in frontend production code", () => {
    const srcRoot = join(REPO_ROOT, "src");
    const prodFiles = walkFiles(srcRoot, [".ts", ".svelte"]).filter(
      (f) => !f.endsWith(".test.ts") && !f.endsWith(".spec.ts"),
    );

    const flagArrayOrPush =
      /(?:\[\s*|push\(\s*)(?:"|')(--resume|--conversation|--session-id|--continue|--fork-session|-r)(?:"|')/g;
    const standaloneFlagLiteral =
      /(?:"|')(--resume|--conversation|--fork-session|--session-id)(?:"|')/g;

    const arrayViolations: string[] = [];
    const literalViolations: string[] = [];

    for (const file of prodFiles) {
      const rel = relative(REPO_ROOT, file).split(sep).join("/");
      const code = stripJsAndSvelteComments(readFileSync(file, "utf8"));

      if (rel === "src/lib/terminal/hostedAgent.ts") {
        // `hostedAgent.ts` may inspect pre-existing caller flags (`SESSION_FLAGS`
        // / `namedSessionId`), never return or push CLI session flags in argv.
        const flagMatch = /"--(?:resume|conversation|session-id|continue|fork-session)"/g;
        for (const line of code.split("\n")) {
          if (!flagMatch.test(line)) continue;
          flagMatch.lastIndex = 0;
          if (line.includes("SESSION_FLAGS") || line.includes("namedSessionId(")) continue;
          arrayViolations.push(`${rel}: ${line.trim()}`);
        }
        continue;
      }

      for (const match of code.matchAll(flagArrayOrPush)) {
        arrayViolations.push(`${rel}: ${match[0]}`);
      }

      for (const match of code.matchAll(standaloneFlagLiteral)) {
        // Ignore human tooltip copy in `title="..."` attributes.
        const idx = match.index ?? 0;
        const lineStart = code.lastIndexOf("\n", idx) + 1;
        const lineEnd = code.indexOf("\n", idx);
        const line = code.slice(lineStart, lineEnd === -1 ? code.length : lineEnd);
        if (line.includes("title={")) continue;
        literalViolations.push(`${rel}: ${match[0]}`);
      }
    }

    expect(arrayViolations, "frontend must never construct resume/session CLI argv arrays").toEqual([]);
    expect(literalViolations, "frontend must send typed `resume: { session_id, fork }` instead of CLI flags").toEqual(
      [],
    );

    const terminalSession = readFileSync(join(srcRoot, "lib/components/TerminalSession.svelte"), "utf8");
    expect(terminalSession).toContain("resume: spawnResume");
    expect(terminalSession).toContain("reopenRowId");
  });

  it("keeps Rust --resume / --conversation / --session-id / --fork-session argv building exclusively inside src-tauri/src/workbench/terminal_command.rs", () => {
    const rustSrcRoot = join(REPO_ROOT, "src-tauri", "src");
    const rustProdFiles = walkFiles(
      rustSrcRoot,
      [".rs"],
      (dir) => dir.endsWith(`${sep}tests`),
    ).filter((f) => !f.endsWith("_tests.rs"));

    const resumeFlagLiteral = /"--(?:resume|conversation|session-id|fork-session)"/g;
    const owners: string[] = [];

    for (const file of rustProdFiles) {
      const rel = relative(REPO_ROOT, file).split(sep).join("/");
      const code = stripRustNonTestCode(readFileSync(file, "utf8"));
      if (resumeFlagLiteral.test(code)) {
        owners.push(rel);
      }
      resumeFlagLiteral.lastIndex = 0;
    }

    expect(owners).toEqual(["src-tauri/src/workbench/terminal_command.rs"]);

    const ownerSource = readFileSync(join(rustSrcRoot, "workbench/terminal_command.rs"), "utf8");
    expect(ownerSource).toContain("pub fn resume(");
    expect(ownerSource).toContain("pub fn session_id_args(");
    expect(ownerSource).toContain("pub fn refuse_raw_session_args(");
    expect(ownerSource).toContain("pub fn interactive_args(");
  });

  it("enforces typed hook payload hardening in the alert bridge and instance IPC", () => {
    const bridge = readFileSync(join(REPO_ROOT, "src-tauri/src/alerts/bridge.rs"), "utf8");
    const reason = readFileSync(join(REPO_ROOT, "src-tauri/src/alerts/bridge/reason.rs"), "utf8");
    const instance = readFileSync(join(REPO_ROOT, "src-tauri/src/desktop/instance.rs"), "utf8");
    expect(bridge).toContain("is_forbidden_argv_key");
    expect(bridge).toContain("contains_forbidden_argv_field");
    expect(reason).toContain("ForbiddenArgvField");
    expect(instance).toContain("deny_unknown_fields");
  });

  it("strips nested markup and block comments whose removal exposes another opener", () => {
    expect(stripJsAndSvelteComments('<!<!---->-- ["--resume"] -->')).toBe("");
    expect(stripJsAndSvelteComments('<!/*x*/-- ["--session-id"] -->')).toBe("");
    expect(stripJsAndSvelteComments('//**/* ["--fork-session"] */')).toBe("");
  });
});
