import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskManviAssist.svelte", import.meta.url), "utf8");

describe("TaskManviAssist", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskManviAssist.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("gates Ask on manviGate and uses the shared local model selection", () => {
    expect(source).toContain("!manviGate.ok");
    expect(source).toContain("effectiveSelection");
    expect(source).toContain("requestManviFocus(\"model\")");
    expect(source).toContain("Pick a local model in Local model servers");
    expect(source).toContain("enhancementConfiguration(liveSelection)");
    expect(source).not.toContain("bind:value={configuration.model}");
    expect(source).not.toContain("Task model settings");
  });

  it("keeps inline accept, compact history, and field locks in one section", () => {
    expect(source).toContain("startQuickEnhance");
    expect(source).toContain("Use this title");
    expect(source).toContain("Use this description");
    expect(source).toContain("history-drawer");
    expect(source).toContain("Keep title");
    expect(source).toContain("Keep description");
    expect(source).toContain("explainEnhancementFailure");
    expect(source).toContain("Resolve uncertain attempt");
    expect(source).toContain('{proposal.state === "running" ? "Cancel" : "Dismiss"}');
  });
});
