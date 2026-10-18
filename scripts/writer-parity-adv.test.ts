import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readCommandsSource } from "./commands-source";

/**
 * Adversarial review of the writer-parity relaxation in
 * command-gate-fidelity-contract.test.ts.
 *
 * The contract moved six commands into DERIVED_ARGV on the strength of "the
 * cmd_* and the writer call the same builder". That claim is itself only a
 * string in a table, so this pins it: each derived command's body must call
 * its builder (and spell no literal "git"), and the writer method must call
 * the same builder. Reintroducing a hand-copied literal then fails here by
 * name, in addition to the generic stale-entry check.
 */
const COMMANDS = readCommandsSource();
const WRITER = readFileSync(
  new URL("../src-tauri/src/engine/git_writer.rs", import.meta.url),
  "utf8",
);

function fnBody(source: string, name: string): string {
  const signature = new RegExp(`fn ${name}\\s*[(<]`).exec(source);
  expect(signature, `fn ${name} must exist`).not.toBeNull();
  const open = source.indexOf("{", signature!.index);
  let depth = 0;
  for (let cursor = open; cursor < source.length; cursor += 1) {
    if (source[cursor] === "{") depth += 1;
    else if (source[cursor] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, cursor + 1);
    }
  }
  throw new Error(`unbalanced braces in ${name}`);
}

const TABLE: Array<[string, string[], string, string[]]> = [
  ["cmd_create_branch", ["create_branch_argv"], "create_branch", ["create_branch_argv"]],
  ["cmd_delete_branch", ["delete_branch_argv"], "delete_branch", ["delete_branch_argv"]],
  ["cmd_rename_branch", ["rename_branch_argv"], "rename_branch", ["rename_branch_argv"]],
  ["cmd_create_tag", ["create_tag_argv"], "create_tag", ["create_tag_argv"]],
  ["cmd_delete_tag", ["delete_tag_argv"], "delete_tag", ["delete_tag_argv"]],
  ["cmd_publish_release", ["create_tag_argv", "push_tag_argv"], "push_tag", ["push_tag_argv"]],
  ["cmd_stage_selective_patch", ["apply_patch_to_index_argv"], "apply_patch_to_index", ["apply_patch_to_index_argv"]],
  ["cmd_unstage_selective_patch", ["apply_patch_to_index_argv"], "apply_patch_to_index", ["apply_patch_to_index_argv"]],
];

describe("derived writer commands really share their builder", () => {
  it.each(TABLE)("%s calls its builder on both sides", (command, cmdBuilders, writerFn, writerBuilders) => {
    const body = fnBody(COMMANDS, command);
    for (const builder of cmdBuilders) {
      expect(body, `${command} must call GitWriter::${builder}`).toContain(`GitWriter::${builder}(`);
    }
    expect(body, `${command} must not hand-copy a "git" literal`).not.toMatch(/"git"/);
    const writer = fnBody(WRITER, writerFn);
    for (const builder of writerBuilders) {
      expect(writer, `${writerFn} must run Self::${builder}`).toContain(`Self::${builder}(`);
    }
  });

  it("no guarded command anywhere still hand-copies a git literal into guard()", () => {
    // Known remaining: cmd_stash_pop (history.rs), a documented owner decision.
    const offenders = [...COMMANDS.matchAll(/guard\(\s*&repo_path,\s*&\[\s*"git"[^\]]*\]/g)].map((m) => m[0]);
    expect(offenders.filter((o) => !o.includes('"stash", "pop"'))).toEqual([]);
  });

  it("a hand-copied delete_tag literal is detectable by the same classifier", () => {
    // In-memory mutation of cmd_delete_tag: the property the contract relies
    // on is that a `"git"` literal in the body reclassifies the command out of
    // the derived set.
    const mutated = fnBody(COMMANDS, "cmd_delete_tag").replace(
      "&GitWriter::delete_tag_argv(&tag_name)",
      '&["git", "tag", "-d", tag_name.as_str()]',
    );
    expect(mutated).toMatch(/"git"/);
    expect(mutated).toContain("GitWriter::delete_tag");
  });
});
