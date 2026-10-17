import { describe, expect, it } from "vitest";
import { readCommandsSource } from "./commands-source";

const source = readCommandsSource();

function functionSource(name: string): string {
  const start = source.indexOf(`pub async fn ${name}(`);
  expect(start, `${name} must exist`).toBeGreaterThanOrEqual(0);
  const end = source.indexOf("\n}\n", start);
  expect(end, `${name} must have a closed body`).toBeGreaterThan(start);
  return source.slice(start, end);
}

/**
 * Every command whose return type is `Guarded<T>`.
 *
 * DERIVED, not listed. The previous version of this contract named five
 * commands by hand, so the eighteen added since were covered by nothing and a
 * new mutation shipped without a gate would have passed. `Guarded<T>` exists
 * precisely to carry a policy verdict to the UI, so returning it IS the
 * declaration that this command mutates and must be judged — which makes it
 * the right thing to enumerate from.
 */
function guardedCommands(): string[] {
  const found = [
    ...source.matchAll(/pub async fn (cmd_\w+)\([^)]*\)\s*->\s*Result<Guarded</gs),
  ].map((match) => match[1]);
  return [...new Set(found)];
}

describe("native mutation policy contract", () => {
  it("finds the guarded command set at all", () => {
    // A regex that silently matches nothing would make every assertion below
    // vacuously true — the exact way a contract test rots into decoration.
    const commands = guardedCommands();
    expect(commands.length).toBeGreaterThan(30);
    for (const anchor of ["cmd_commit", "cmd_push", "cmd_reset", "cmd_stash_action"]) {
      expect(commands, `${anchor} must be among the guarded commands`).toContain(anchor);
    }
  });

  it("routes every policy-carrying command through the write gate", () => {
    for (const name of guardedCommands()) {
      let body = functionSource(name);
      // A command that hands its body to a named sync delegate (so the gate
      // is testable without the async runtime) is judged by that delegate's
      // body, which must itself call the gate: `docs_rename_guarded` judges
      // the move and every path the rename writes.
      const delegate = /\b(docs_rename_guarded)\(/.exec(body)?.[1];
      if (delegate) {
        const start = source.indexOf(`fn ${delegate}(`);
        expect(start, `${name} delegates to ${delegate}, which must exist`).toBeGreaterThanOrEqual(0);
        body = source.slice(start, source.indexOf("\n}\n", start));
        // Each rewritten path is judged on its own path: one at a time, or as
        // one batch of per-path verdicts (`guard_files`), every one recorded.
        const perPath = body.includes('guard_file(repo_path, path, "modify")')
          || (body.includes('guard_files(repo_path, &rewrites, "modify")') && /for verdict in guard_files\([^)]*\)\?\s*\{\s*fold\.record\(verdict\)/.test(body));
        expect(perPath, `${delegate} must judge each path it rewrites`).toBe(true);
      }
      // `guard` judges a rendered command line; `guard_file` judges a path.
      // Both are the canonical harness entry points; anything else is a
      // command that reports a verdict it never actually obtained.
      // `guard_move` is the move delegate: it judges a MovePlan with exactly
      // those two, which the next test pins so it cannot become a bypass.
      // `guard_background` is the same gate for a command nobody clicked (the
      // auto-fetch timer); the next-but-one test pins that it is a real call.
      const judged = body.includes("guard(") || body.includes("guard_file(") || body.includes("guard_move(")
        || body.includes("guard_background(");
      expect(judged, `${name} returns Guarded<..> but never calls the write gate`).toBe(true);
    }
  });

  it("keeps the move delegate a real gate call for both kinds of move", () => {
    const start = source.indexOf("fn guard_move(");
    expect(start).toBeGreaterThanOrEqual(0);
    const body = source.slice(start, source.indexOf("\n}\n", start));
    expect(body).toContain("guard(repo_path, &refs)");
    expect(body).toContain('guard_file(repo_path, from, "delete")');
    expect(body).toContain('guard_file(repo_path, to, "modify")');
  });

  it("keeps the background delegate a real gate call", () => {
    const start = source.indexOf("fn guard_background(");
    expect(start).toBeGreaterThanOrEqual(0);
    const body = source.slice(start, source.indexOf("\n}\n", start));
    expect(body).toContain("crate::harness::guard_command(repo_path, argv)");
  });

  it("keeps the historically hand-listed commands covered", () => {
    // Regression guard for the derivation itself: these five were the whole
    // contract before it was derived, so losing them means the regex broke.
    const commands = guardedCommands();
    for (const name of [
      "cmd_stage_file",
      "cmd_unstage_file",
      "cmd_fetch",
      "cmd_stash_save",
      "cmd_stash_pop",
    ]) {
      expect(commands).toContain(name);
      expect(functionSource(name)).toContain("guard(");
    }
  });
});
