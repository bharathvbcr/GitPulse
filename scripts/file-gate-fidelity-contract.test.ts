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

describe("conflict file authorization", () => {
  // The operation itself (create / modify / delete / stage-deletion) is derived
  // from the checked source in `conflict_session::save_with_gate` and driven
  // through a real repository by `file_authorization_distinguishes_creation_modification_and_deletion`
  // (tests/conflict_save_integration.rs); the folding of the file verdict into
  // the result is `conflict_save_surfaces_the_file_verdict_not_only_the_last_command`.
  // What only the source can show is that the Tauri command forwards that
  // derived operation to the gate rather than declaring one of its own.
  it("forwards the derived operation to the file gate instead of declaring one", () => {
    const body = fnBody(production(COMMANDS), "cmd_save_conflict");
    expect(body).not.toBeNull();
    const call = /guard_file\(\s*&repo_path\s*,\s*(\w+)\s*,\s*([^)]+?)\)/s.exec(body!);
    expect(call, "cmd_save_conflict no longer reaches guard_file").not.toBeNull();
    expect(call![2].trim(), "a quoted operation here would gate every conflict save as one fixed act").not.toMatch(/^"/);
  });
});

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

  const destructiveSites = sites.filter((site) =>
    site.delegates
      .flatMap((delegate) => writerSubcommands(delegate))
      .some((subcommand) => DESTRUCTIVE_SUBCOMMANDS.includes(subcommand)),
  );

  it("has command sites whose writer can delete, so the rule below is not vacuous", () => {
    expect(destructiveSites.map((site) => site.command)).toContain("cmd_discard_changes");
  });

  it.each(destructiveSites.map((site) => [site.command, site] as const))(
    "%s does not gate a possible delete as a mere modify",
    (_name, site) => {
      // The writer can remove the path, so the op cannot be a hardcoded weaker
      // literal; it has to be derived from what will actually happen.
      expect(
        site.op,
        `${site.command} delegates to a writer that can run git ${DESTRUCTIVE_SUBCOMMANDS.join("/")} ` +
          `but declares a fixed op ${site.op}; a delete gated as a modify is judged ` +
          `and recorded as something it is not`,
      ).not.toMatch(/^"(modify|write|create)"$/);
    },
  );
});
