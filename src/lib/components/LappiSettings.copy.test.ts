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

  it("says where it goes, and that it is not an AI provider", () => {
    expect(description).toContain("local socket");
    expect(description).toContain("not to an AI provider");
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
