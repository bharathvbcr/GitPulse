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

  it("surfaces why Cmd+Enter cannot start a draft instead of returning silently", () => {
    expect(source).toContain("if (askDisabled && !quick)");
    // The reason now depends on the engine: Manvi's is about a model server
    // that has to be running, Apple's about a Mac setting or a size limit.
    // Reporting Manvi's reason while Apple is selected would send the reader
    // to the wrong settings pane.
    expect(source).toContain('error = gate ?? (engine === "apple" ? appleAsk.reason : fieldReason)');
  });

  it("offers the on-device engine only where the bridge exists, and never silently", () => {
    // Three separate facts, deliberately not collapsed: a build without the
    // bridge hides the picker entirely; a Mac that could run it but is not set
    // up shows the option disabled with the framework's own reason; and a
    // selected-but-unavailable engine falls back to the engine every build has,
    // rather than leaving a dead button.
    expect(source).toContain("const appleOffered = $derived(Boolean(apple?.compiled))");
    expect(source).toContain(
      'const engine = $derived<AssistEngine>(requestedEngine === "apple" && appleReady(apple) ? "apple" : DEFAULT_ASSIST_ENGINE)',
    );
    // Names come from the engine table, never spelled here: this section and
    // the sheet around it both write the running engine's name, and a literal
    // in either is how they came to disagree.
    expect(source).toContain("const engineName = $derived(assistEngineName(engine))");
    expect([...source.matchAll(/"Apple Intelligence"|>Apple Intelligence</g)]).toEqual([]);
    expect(source).toContain("{#if appleOffered}");
    expect(source).toContain("disabled={disabled || acting || !appleReady(apple)}");
    expect(source).toContain("{apple?.detail}");
    // Both engines create the proposal in the same store, so the "one live
    // attempt" check must not sit inside either branch.
    expect(source).toMatch(/const page = await bounded\(listEnhancements\(saved\.id\)\)[\s\S]*?runAppleEnhancement/);
  });

  it("publishes its suggestion instead of drawing its own copy of the fields", () => {
    /*
     * The assist used to carry a second Title and Description input and an
     * "Use this title" button beside them, so a reader comparing a suggestion
     * to what they wrote was looking at two pairs of fields in one sheet.
     * It now owns the lifecycle and hands the *result* to the editor, which
     * draws each suggestion under the field it would replace. The two exported
     * functions are that seam: without them the editor's buttons do nothing.
     */
    expect(source).toContain("export function acceptFields");
    expect(source).toContain("export function hideSuggestion");
    expect(source).toContain("onSuggestion?: (state: AssistSuggestion) => void");
    expect(source).not.toContain("Use this title");
    expect(source).not.toContain("Use this description");
    expect(source).not.toMatch(/<label[^>]*>\s*Title/);
  });

  it("keeps quick enhance, compact history, and field locks in one section", () => {
    expect(source).toContain("startQuickEnhance");
    expect(source).toContain("history-drawer");
    expect(source).toContain("Keep title");
    expect(source).toContain("Keep description");
    expect(source).toContain("explainEnhancementFailure");
    expect(source).toContain("Resolve uncertain attempt");
    expect(source).toContain('{proposal.state === "running" ? "Cancel" : "Dismiss"}');
  });
});
