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

  // Rendered in harness/tasksChecks.js: the engine is offered beside Manvi on
  // a Mac that has it, renames the action and says where it runs, is offered
  // but not selectable when switched off, and is absent from a build with no
  // bridge; its draft is one proposal in the same store, reviewed and accepted
  // exactly like a Manvi one. What stays is what the fixture cannot show.
  it("names the on-device engine from the engine table, never a literal here", () => {
    expect(source).toContain("const engineName = $derived(assistEngineName(engine))");
    expect([...source.matchAll(/"Apple Intelligence"|>Apple Intelligence</g)]).toEqual([]);
  });

  it("checks for one live attempt before branching on the engine", () => {
    // Both engines create the proposal in the same store; the fixture never
    // holds a live attempt while the on-device engine is chosen.
    expect(source).toMatch(/const existing = await bounded\(latestEnhancement\(saved\.id, LIVE_ENHANCEMENT_STATES\)\)[\s\S]*?runAppleEnhancement/);
  });
  // Compose copy on a new task and improve copy on a saved one are rendered
  // in harness/tasksChecks.js ("a new task's assist asks what is needed...",
  // "a saved task's assist offers to improve it...").
  it("carries no fixture text into the shipped copy", () => {
    expect(source).not.toContain("Keep the original E42 across both repository links");
  });
  it("tells the sheet only which fields changed and the attempt's state", () => {
    // Acceptance living once, in the review beside the diff, with no "Use
    // this title" under the fields, is rendered by "acceptance lives once, in
    // the review that shows the diff". This is the seam's shape: the only
    // things crossing it are the flash and the status.
    expect(source).toContain("onFlash?: (fields: EnhancementField[]) => void");
    expect(source).toContain("$effect(() => { onFlash([...flash]); })");
    expect(source).toContain("onStatus?: (status: { state: string | null; uncertain: boolean }) => void");
    expect(source).toContain("onStatus({ state: proposal?.state ?? null, uncertain: needsReconcile })");
  });
  it("keeps quick enhance, suggestion history, and field locks in one section", () => {
    expect(source).toContain("startQuickEnhance");
    expect(source).toContain("Keep title");
    expect(source).toContain("Keep description");
    expect(source).toContain("explainEnhancementFailure");
    expect(source).toContain("Resolve uncertain attempt");
    expect(source).toContain('{proposal.state === "running" ? "Cancel" : "Dismiss"}');
  });

  it("flashes the fields the store says changed, on the path that actually accepts", () => {
    /*
     * The flash used to be set by a second accept function the sheet called
     * directly. Removing that left `mutate` — the one path acceptance now
     * takes — setting no flash at all, so the cue would have been permanently
     * dead while still looking wired. It reads `accepted_fields` off the
     * result rather than the selection it sent, because the flash is a claim
     * about what changed.
     */
    expect(source).toMatch(/if \(method === "enhancements\.accept"\) \{[\s\S]*?flash = \[\.\.\.changed\]/);
    expect(source).toContain("const changed = proposal.accepted_fields.filter(");
    expect(source).toMatch(/flashTimer = setTimeout\(\(\) => \{ flash = \[\]; \}, 1600\)/);
    // And exactly one function writes it.
    expect([...source.matchAll(/flash = \[\.\.\./g)]).toHaveLength(1);
  });

  // The unsaved-edits refusal and its sentence are rendered ("acceptance
  // refuses to overwrite unsaved task edits, and says why"); the suggestion
  // history picker, changing the selection, the page-loaded line and field
  // selection not dirtying the task are rendered in the same harness.
  it("says why a suggestion for an older revision is refused, from the same expression as the button", () => {
    // The fixture never serves a suggestion for an older revision.
    expect(source).toContain("This suggestion is for an older task revision. Ask again to apply it.");
    expect(source).toContain("disabled={Boolean(acceptBlock) || disabled || controlsLocked || !selected.length}");
  });
  it("says why a selected suggestion cannot be applied, instead of hiding the button", () => {
    // Picking an older entry and finding no Apply was the drawer's quietest
    // dead end: the store refuses on `expected_task_revision`, so the refusal
    // is a fact to state, not an absence to render.
    expect(source).toContain("enhancementApplyBlock");
    expect(source).toContain("{#if applyBlock}");
  });

  it("holds an auto-start request until the surface can act on it", () => {
    /*
     * `startEnhancement` refuses while `disabled` and says nothing, so an
     * effect that consumed the request first would drop the draft with no
     * trace. The condition therefore carries `disabled` itself, which also
     * makes it a dependency: the request survives until the surface is ready.
     */
    expect(source).toContain("if (active && !disabled && startRequest > lastStart)");
    expect(source).toMatch(/lastStart = startRequest;[\s\S]*?startEnhancement\(\)/);
  });

  it("lets concurrent callers await the same configuration read", () => {
    /*
     * Two things ask for configuration the moment a surface opens to start
     * work: the selection effect, and `startEnhancement`. The old guard
     * returned early for the second — reporting "loaded" while the request was
     * still in flight — and it keyed on `acting`, which `startEnhancement`
     * itself sets via `preparing` around its own call. Between them the
     * auto-start could never read a configuration and silently did nothing.
     *
     * Only the browser harness can catch the behaviour (vitest runs SSR, so no
     * `$effect` ever fires). This pins the shape that makes it correct.
     */
    expect(source).toContain("configLoad ??=");
    expect(source).toMatch(/configLoad = null/);
    expect(source).toContain("if (busy || needsReconcile || disabled) return Promise.resolve()");
    // The bound on the coalescing loop: a selection that never settles must
    // not spin the request forever.
    expect(source).toMatch(/for \(let attempt = 0; attempt < \d+; attempt\+\+\)/);
  });

  it("re-reads the history only when a polled suggestion moved", () => {
    // ft-0ff882a6: each one-second poll of a live suggestion also re-read the
    // history (two requests) whether or not anything changed.
    expect(source).toContain("if (!enhancementMoved(proposal, next)) return;");
    expect(source).toContain("void follow(id); }, 2000)");
    expect(source).toContain("if (disposed || latestActive?.id !== id || !enhancementMoved(latestActive, next)) return;");
    // Both loops stay on the visibility-aware timer.
    expect(source).not.toMatch(/\bsetInterval\(/);
    expect(source).toContain("return createAdaptiveTimer(() => { void poll(id); }, 1000);");
    expect(source).toContain("return createAdaptiveTimer(() => { if (!acting && !editing) void follow(id); }, 2000);");
  });
});
