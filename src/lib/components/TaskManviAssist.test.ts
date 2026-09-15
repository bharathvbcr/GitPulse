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

  it("keeps quick enhance, suggestion history, and field locks in one section", () => {
    expect(source).toContain("startQuickEnhance");
    expect(source).toContain("Keep title");
    expect(source).toContain("Keep description");
    expect(source).toContain("explainEnhancementFailure");
    expect(source).toContain("Resolve uncertain attempt");
    expect(source).toContain('{proposal.state === "running" ? "Cancel" : "Dismiss"}');
  });

  it("picks a past suggestion with a dropdown, not a collapsed drawer", () => {
    /*
     * History used to be a `<details>` holding a 150px scroller of buttons.
     * Three things were wrong with it at once, and only the third was fatal:
     * the list was folded away by default, its rows carried no time so two
     * runs of one model read identically, and the review it selected was
     * suppressed whenever a ready suggestion was already showing beside the
     * fields — so changing the selection could do nothing visible at all.
     */
    expect(source).toContain('data-testid="task-assist-history"');
    expect(source).toContain("enhancementOptionLabel");
    expect(source).not.toContain("history-drawer");
    expect(source).not.toContain("historyOpen");
  });

  it("always renders the review for whatever suggestion is selected", () => {
    // The suppression this replaces read
    // `quick || historyOpen || proposal.state !== "ready" || (!showTitle…)`.
    expect(source).not.toContain("historyOpen ||");
    expect(source).toMatch(/\{#if proposal\}\s*\n\s*<article aria-label="Enhancement review">/);
    // The duplicate-acceptance problem that condition was really solving is
    // now solved without hiding the diff.
    expect(source).toContain("const reviewOffersAccept = $derived(");
    expect(source).toMatch(/\{#if reviewOffersAccept\}[\s\S]*?enhancements\.accept/);
  });

  it("never shows a suggestion the review is not rendering", () => {
    // A `<select>` displays the reader's pick immediately; loading it is a
    // round trip that can fail or be superseded. Without its own state the
    // control would name a revision the diff below is not showing.
    expect(source).toContain("let selectedId = $state");
    expect(source).toContain("const ticket = ++selecting");
    expect(source).toMatch(/finally \{[\s\S]*?selectedId = proposal\?\.id \?\? ""/);
    // One sync point for the five other paths that replace `proposal`.
    expect(source).toMatch(/if \(!selectPending\) selectedId = id/);
  });

  it("says why a selected suggestion cannot be applied, instead of hiding the button", () => {
    // Picking an older entry and finding no Apply was the drawer's quietest
    // dead end: the store refuses on `expected_task_revision`, so the refusal
    // is a fact to state, not an absence to render.
    expect(source).toContain("enhancementApplyBlock");
    expect(source).toContain("{#if applyBlock}");
  });

  it("reports the page it loaded rather than implying the list is complete", () => {
    expect(source).toContain("Showing {entries.length} of {total}");
    expect(source).toContain("Load more");
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
});
