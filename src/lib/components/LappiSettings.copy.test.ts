/**
 * What the Lappi settings say about the data they move.
 *
 * "Ask Lappi" sends the staged diff, whole, to the local Lappi process
 * (`src-tauri/src/lappi/request.rs`: `context_b64` is the diff). A switch that
 * says only "ask a running agent" leaves that out, and the diff is the one
 * thing a person deciding whether to turn it on needs to know travels. Source
 * read, like the other copy contracts here: the wording is the contract.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "LappiSettings.svelte"), "utf8");

/** The `description` attribute of the toggle whose `label` is `label`. */
function descriptionOf(label: string): string {
  const at = source.indexOf(`label="${label}"`);
  expect(at, `no toggle labelled "${label}"`).toBeGreaterThan(0);
  const match = /description="([^"]*)"/.exec(source.slice(at));
  expect(match, `"${label}" has no description`).not.toBeNull();
  return match![1];
}

describe("the Ask Lappi switch", () => {
  const description = descriptionOf("Ask Lappi on ambiguous commit types");

  it("says the staged diff is sent to the local Lappi process", () => {
    expect(description).toContain("staged diff is sent to the local Lappi process");
  });

  it("says where it goes, and that AI commit-message generation is separate", () => {
    expect(description).toContain("local socket");
    expect(description).toContain("separate from AI commit-message generation");
    expect(description).toContain("has its own consent");
  });

  // The first version of this file pinned "GitPulse sends it nowhere else".
  // That sentence is false of GitPulse: `generate_commit_message` sends the
  // same diff to the confirmed model server under the AI consent gate. The
  // switch's own claim is the only one it may make, so the overclaim is a
  // regression, not wording.
  it("does not claim the diff goes nowhere else", () => {
    expect(description).not.toMatch(/nowhere else/i);
    expect(description).not.toMatch(/not to an AI provider/i);
  });

  it("keeps what it already said: off by default, and the answer only pre-selects", () => {
    expect(description).toContain("Off by default");
    expect(description).toContain("pre-select the type");
  });
});

describe("the record switch", () => {
  it("still says it keeps no patch or message and uploads nothing", () => {
    const description = descriptionOf("Record Lappi caller data (local only)");
    expect(description).toContain("never the patch or the message");
    expect(description).toContain("Nothing is uploaded");
  });
});
