/**
 * Adversarial check on lane L5's Lappi copy (ft-878db72b), written by a
 * reviewer who did not write it. A failing test here is a claim that is false.
 *
 * The "Ask Lappi" switch says "GitPulse sends it nowhere else". The staged diff
 * it names is the same one `generate_commit_message` (`src-tauri/src/ai/mod.rs`)
 * goes on to give a model server: `consult_on_commit_type(repo_path, &patch, ..)`
 * is called from inside that function, and the next step is `resolve_target`,
 * "the one gate between 'a model server is running' and 'send it the diff'",
 * which sends the diff, budgeted, to the confirmed server. So the sentence is
 * true of this switch and false of GitPulse, and a person deciding about the
 * switch reads it as the second.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "LappiSettings.svelte"), "utf8");

function descriptionOf(label: string): string {
  const at = source.indexOf(`label="${label}"`);
  expect(at, `no toggle labelled "${label}"`).toBeGreaterThan(0);
  const match = /description="([^"]*)"/.exec(source.slice(at));
  expect(match, `"${label}" has no description`).not.toBeNull();
  return match![1];
}

describe("the Ask Lappi switch copy", () => {
  it("adv_l5_does_not_claim_GitPulse_sends_the_diff_nowhere_else", () => {
    const description = descriptionOf("Ask Lappi on ambiguous commit types");
    // The subject is what is false: a scoped "this switch sends it nowhere
    // else" would be true and is not what this pins.
    expect(description).not.toMatch(/GitPulse sends it nowhere else/);
  });
});
