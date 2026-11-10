import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { functionBody } from "../stores/sourceScan";
import LocalModelServers from "./LocalModelServers.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "LocalModelServers.svelte"),
  "utf8",
);

describe("LocalModelServers", () => {
  it("renders the card heading and the Claude model control it sits with", () => {
    const { body } = render(LocalModelServers);
    expect(body).toContain("Local model servers");
    expect(body).toContain("Manvi's model");
    expect(body).not.toContain("Harness &amp; AI");
  });

  it("reports a failed sweep instead of presenting it as an empty machine", () => {
    const scan = functionBody(source, "runScan");
    expect(scan).toContain('scanError = failure("scan", "local model servers", e);');
    expect(scan).toContain("scan = null;");
    expect(source).toContain("This is not a list of what is running.");
  });

  it("pins the chosen model through the store, and can clear the pin", () => {
    expect(functionBody(source, "pick")).toContain("harnessStore.selectModel(selection)");
    expect(source).toContain("pick({ base_url: server.base_url, model: model.id })");
    expect(source).toContain("pick({ base_url: endpoint.base_url, model })");
    expect(source).toContain("onclick={() => pick(null)}");
  });

  it("does not exclude a model on a capability flag nobody asked for", () => {
    const usable = functionBody(source, "usable");
    expect(usable).toContain("!model.capabilities_known || model.supports_completion");
    // Three states: unknown is never rendered as "no tools".
    expect(source).toContain('!model.capabilities_known ? "unknown"');
  });

  it("prefers the pinned model over discovery when marking the selection", () => {
    const selected = functionBody(source, "isSelected");
    expect(selected.indexOf("preferred")).toBeLessThan(selected.indexOf("ai?.selected"));
  });

  it("keeps keys unique when a server lists the same model twice", () => {
    expect(source).toContain("{#each scan.servers as server, i (`${server.base_url}#${i}`)}");
    expect(source).toContain('{#each server.models as model, j (`${model.id || "model"}#${j}`)}');
  });
});
