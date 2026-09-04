import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The command gate must judge the command that actually runs.
 *
 * A guarded handler builds an argv by hand, hands it to `guard()`, and then
 * calls a `GitWriter` that builds its own argv independently. Nothing made the
 * two agree. `command-policy-contract.test.ts` proves a gate is *present*;
 * this proves the gate was told the truth. Drift here does not fail loudly —
 * the verdict still renders as authoritative while describing a command that
 * never ran, which is the same shape as a check that could not run reading
 * like one that passed.
 *
 * Scope, stated plainly so this is not over-trusted: it is a CONTAINMENT check
 * over literal flags, not a proof of argv equality. Every literal flag the gate
 * declares must appear somewhere in the writer it delegates to. That catches
 * the realistic drift — a writer that stops passing `--force-with-lease` while
 * the gate still promises it — and cannot catch flags assembled at runtime from
 * non-literal values.
 *
 * Commands whose argv is derived rather than literal (`cmd_reset`,
 * `cmd_rebase_interactive`, the worktree and GitHub families) are compared by
 * their own mirror tests instead, and are listed below rather than silently
 * skipped: a skip nobody can see is how this rots into decoration.
 */
const COMMANDS = readFileSync(
  new URL("../src-tauri/src/commands/mod.rs", import.meta.url),
  "utf8",
);
const WRITER = readFileSync(
  new URL("../src-tauri/src/engine/git_writer.rs", import.meta.url),
  "utf8",
);

/**
 * Guarded commands deliberately outside the literal comparison, and why.
 * A NEW unparseable command fails instead of joining this list quietly.
 */
const DERIVED_ARGV = Object.freeze({
  cmd_rebase_interactive: "derives the planned sequence; rebase_planned_commands_* mirror tests cover it",
  cmd_stash_action: "argv built from the selected stash OID at runtime",
  cmd_cherry_pick: "shares replay_argv with the writer, so drift is impossible",
  cmd_revert: "shares replay_argv with the writer, so drift is impossible",
  cmd_reset: "shares reset_argv with the writer, so drift is impossible",
  cmd_remote_change: "argv varies by RemoteChange variant",
  cmd_submodule_change: "argv varies by SubmoduleChange variant",
  cmd_repo_operation_action: "argv varies by OperationAction variant",
  cmd_github_create_issue: "shells out to gh, not git",
  cmd_github_checkout_pr: "shells out to gh, not git",
  cmd_github_trigger_workflow: "shells out to gh, not git",
  cmd_github_rerun_run: "shells out to gh, not git",
  cmd_github_cancel_run: "shells out to gh, not git",
  cmd_add_worktree: "worktree argv is built in engine::worktree, not git_writer",
  cmd_remove_worktree: "worktree argv is built in engine::worktree, not git_writer",
  cmd_lock_worktree: "worktree argv is built in engine::worktree, not git_writer",
  cmd_unlock_worktree: "worktree argv is built in engine::worktree, not git_writer",
  cmd_prune_worktree: "worktree argv is built in engine::worktree, not git_writer",
  cmd_write_file_content: "writes through the sandbox, gated by path not command",
  cmd_discard_changes: "gated by path not command; covered by file-gate-fidelity-contract",
});

function production(source: string): string {
  const marker = source.search(/#\[cfg\(test\)\]\s*\nmod tests/);
  return marker === -1 ? source : source.slice(0, marker);
}

/** Body of `fn name`, brace-matched from its signature. */
function fnBody(source: string, name: string): string | null {
  const signature = new RegExp(`fn ${name}\\s*[(<]`).exec(source);
  if (!signature) return null;
  const open = source.indexOf("{", signature.index);
  if (open === -1) return null;
  let depth = 0;
  for (let cursor = open; cursor < source.length; cursor += 1) {
    if (source[cursor] === "{") depth += 1;
    else if (source[cursor] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, cursor + 1);
    }
  }
  return null;
}

const PRODUCTION_COMMANDS = production(COMMANDS);
const PRODUCTION_WRITER = production(WRITER);

const flagsIn = (body: string): string[] =>
  [...new Set([...body.matchAll(/"(-[^"\\]*)"/g)].map((match) => match[1]))].sort();

/**
 * Every literal flag a writer can pass, following `Self::`/`GitWriter::`
 * helpers transitively so a private helper holding the flags (as
 * `commit_inner` does) is not mistaken for a writer that lost them.
 */
function writerFlags(name: string, seen = new Set<string>()): string[] {
  if (seen.has(name)) return [];
  seen.add(name);
  const body = fnBody(PRODUCTION_WRITER, name);
  if (!body) return [];
  const nested = [...body.matchAll(/(?:Self|GitWriter)::(\w+)/g)].flatMap((match) =>
    writerFlags(match[1], seen),
  );
  return [...flagsIn(body), ...nested];
}

interface GuardedCommand {
  name: string;
  gateFlags: string[];
  delegates: string[];
}

function literalArgvCommands(): { compared: GuardedCommand[]; derived: string[] } {
  const compared: GuardedCommand[] = [];
  const derived: string[] = [];
  for (const match of PRODUCTION_COMMANDS.matchAll(
    /pub async fn (cmd_\w+)\([^)]*\)\s*->\s*Result<Guarded</gs,
  )) {
    const name = match[1];
    const body = fnBody(PRODUCTION_COMMANDS, name);
    if (!body) continue;
    const delegates = [
      ...new Set([...body.matchAll(/GitWriter::(\w+)/g)].map((d) => d[1])),
    ].filter((delegate) => fnBody(PRODUCTION_WRITER, delegate) !== null);
    // A literal `"git"` in the body is what marks a hand-built argv.
    if (!/"git"/.test(body) || delegates.length === 0) {
      derived.push(name);
      continue;
    }
    compared.push({ name, gateFlags: flagsIn(body), delegates });
  }
  return { compared, derived };
}

describe("the command gate judges the command that actually runs", () => {
  const { compared, derived } = literalArgvCommands();

  it("compares a real share of the guarded commands", () => {
    // Vacuity guard: a parser that silently stopped matching would make every
    // comparison below pass while examining nothing.
    expect(compared.length, "no guarded commands were parsed for comparison").toBeGreaterThanOrEqual(15);
    for (const anchor of ["cmd_push", "cmd_commit", "cmd_delete_branch", "cmd_fetch"]) {
      expect(compared.map((c) => c.name), `${anchor} must be compared`).toContain(anchor);
    }
  });

  it("leaves no command silently uncompared", () => {
    // Every command not compared must say why. A new one that cannot be parsed
    // fails here instead of quietly falling out of the contract.
    const undocumented = derived.filter((name) => !(name in DERIVED_ARGV));
    expect(
      undocumented,
      `these guarded commands are neither compared nor documented as derived: ${undocumented.join(", ")}`,
    ).toEqual([]);
  });

  it("keeps the derived-argv list free of stale entries", () => {
    // An entry for a command that no longer exists (or is now comparable) would
    // let a real gap hide behind a justification that stopped applying.
    const stale = Object.keys(DERIVED_ARGV).filter((name) => !derived.includes(name));
    expect(stale, `stale derived-argv entries: ${stale.join(", ")}`).toEqual([]);
  });

  it.each(literalArgvCommands().compared.map((c) => [c.name, c] as const))(
    "%s declares no flag its writer never passes",
    (_name, command) => {
      const writer = new Set(command.delegates.flatMap((delegate) => writerFlags(delegate)));
      const promised = command.gateFlags.filter((flag) => !writer.has(flag));
      expect(
        promised,
        `${command.name} tells the gate ${promised.join(", ")} but ` +
          `${command.delegates.join("/")} never passes it — the verdict describes a command that does not run`,
      ).toEqual([]);
    },
  );
});
