import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The file gate must be told the operation that will actually happen.
 *
 * `guard_file(repo, path, op)` sends `op` to the policy sidecar as
 * `policy.check.file`'s `op` parameter, and records it in the ledger as
 * `file.<op>`. So a command that declares "modify" for a path it may DELETE
 * asks the gate to judge, and the durable record to state, a gentler act than
 * the one that runs — the same shape as every other defect this codebase
 * treats as a bug: a weaker claim standing in for the real one.
 *
 * This is not hypothetical. `cmd_discard_changes` declared "modify"
 * unconditionally while `GitWriter::discard_changes` runs BOTH `git restore`
 * and `git clean -f` against the path. For a tracked file the restore acts and
 * "modify" is true; for an UNTRACKED file the clean acts and the file is
 * removed. Every discard of an untracked file was gated and recorded as a
 * modification.
 *
 * The rule below is derived from the writers, not from a list of known-bad
 * commands, so a newly destructive writer is caught the day it lands.
 */
const COMMANDS = readFileSync(
  new URL("../src-tauri/src/commands/mod.rs", import.meta.url),
  "utf8",
);
const WRITER = readFileSync(
  new URL("../src-tauri/src/engine/git_writer.rs", import.meta.url),
  "utf8",
);

/** Git subcommands that can remove a file from the working tree. */
const DESTRUCTIVE_SUBCOMMANDS = ["clean", "rm"];

/** Source with the `#[cfg(test)] mod tests` block removed. */
function production(source: string): string {
  const marker = source.search(/#\[cfg\(test\)\]\s*\nmod tests/);
  return marker === -1 ? source : source.slice(0, marker);
}

/** Body of `fn name`, brace-matched from its signature. */
function fnBody(source: string, name: string): string | null {
  const signature = new RegExp(`fn ${name}\\s*[(<]`).exec(source);
  if (!signature) return null;
  let index = source.indexOf("{", signature.index);
  if (index === -1) return null;
  let depth = 0;
  for (let cursor = index; cursor < source.length; cursor += 1) {
    if (source[cursor] === "{") depth += 1;
    else if (source[cursor] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(index, cursor + 1);
    }
  }
  return null;
}

/**
 * Every `guard_file` call in a command, with the op expression as written and
 * the writer functions that command delegates to.
 */
interface FileGateSite {
  command: string;
  op: string;
  delegates: string[];
}

function fileGateSites(): FileGateSite[] {
  const source = production(COMMANDS);
  const sites: FileGateSite[] = [];
  for (const match of source.matchAll(
    /pub async fn (cmd_\w+)\s*\(/g,
  )) {
    const body = fnBody(source, match[1]);
    if (!body) continue;
    for (const call of body.matchAll(/guard_file\(\s*&repo_path\s*,\s*&file_path\s*,\s*([^)]+?)\)/gs)) {
      sites.push({
        command: match[1],
        op: call[1].trim(),
        delegates: [...new Set([...body.matchAll(/GitWriter::(\w+)/g)].map((d) => d[1]))],
      });
    }
  }
  return sites;
}

/** Literal git subcommands a writer runs, following one level of helpers. */
function writerSubcommands(name: string, seen = new Set<string>()): string[] {
  if (seen.has(name)) return [];
  seen.add(name);
  const body = fnBody(production(WRITER), name);
  if (!body) return [];
  const own = [...body.matchAll(/git_\w+\(\s*&?\w+\s*,\s*&\[\s*"([^"]+)"/g)].map((m) => m[1]);
  const nested = [...body.matchAll(/(?:Self::|GitWriter::)(\w+)/g)].flatMap((m) =>
    writerSubcommands(m[1], seen),
  );
  return [...own, ...nested];
}

describe("the file gate is told the operation that actually runs", () => {
  const sites = fileGateSites();

  it("finds the guard_file call sites at all", () => {
    // A regex that matched nothing would make every assertion below vacuously
    // true — the exact way a contract test rots into decoration.
    expect(sites.length, "no guard_file call sites parsed").toBeGreaterThanOrEqual(2);
    expect(sites.map((site) => site.command)).toContain("cmd_discard_changes");
  });

  it("finds a writer that can actually delete", () => {
    // Likewise: if writer parsing broke, the rule below would protect nothing.
    const destructive = writerSubcommands("discard_changes");
    expect(destructive, "discard_changes' subcommands not parsed").toContain("clean");
  });

  it.each(
    fileGateSites().map((site) => [site.command, site] as const),
  )("%s does not gate a possible delete as a mere modify", (_name, site) => {
    const subcommands = site.delegates.flatMap((delegate) => writerSubcommands(delegate));
    const destructive = subcommands.filter((subcommand) =>
      DESTRUCTIVE_SUBCOMMANDS.includes(subcommand),
    );
    if (destructive.length === 0) return;
    // The writer can remove the path, so the op cannot be a hardcoded weaker
    // literal — it has to be derived from what will actually happen.
    expect(
      site.op,
      `${site.command} delegates to a writer running \`git ${destructive.join(", ")}\` ` +
        `but declares a fixed op ${site.op}; a delete gated as a modify is judged ` +
        `and recorded as something it is not`,
    ).not.toMatch(/^"(modify|write|create)"$/);
  });
});
