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

  it("speaks compose copy only when the sheet says this is a new task", () => {
    expect(source).toContain("compose = false");
    expect(source).toContain('{compose ? "What do you need?" : "Improve this task"}');
    expect(source).toContain("Notes become a title and description you accept below.");
    expect(source).toContain("Ask for a better title and description, or paste notes to rewrite them.");
    expect(source).not.toContain("Keep the original E42 across both repository links");
  });

  it("owns acceptance rather than publishing it to the sheet", () => {
    /*
     * Three arrangements have been tried. The assist used to carry its own
     * second Title and Description inputs; then it handed the suggestion up to
     * the sheet, which drew "Use this title" beside each field and left the
     * review with no buttons. Both split one decision across two places, and
     * the second made the history picker able to change nothing visible.
     *
     * Acceptance now lives exactly once, beside the diff. The only thing that
     * still crosses the seam is which fields just changed, so the sheet can
     * flash them.
     */
    expect(source).toContain("onFlash?: (fields: EnhancementField[]) => void");
    expect(source).toContain("$effect(() => { onFlash([...flash]); })");
    expect(source).toContain("onStatus?: (status: { state: string | null; uncertain: boolean }) => void");
    expect(source).toContain("onStatus({ state: proposal?.state ?? null, uncertain: needsReconcile })");
    expect(source).not.toContain("acceptFields");
    expect(source).not.toContain("hideSuggestion");
    expect(source).not.toContain("AssistSuggestion");
    // And still no second copy of the fields themselves.
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
    // Nothing between the picker and the diff. `reviewOffersAccept` was the
    // last condition that could leave a selected suggestion showing no
    // buttons; acceptance is unconditional now, so changing the dropdown
    // always changes what is on screen.
    expect(source).not.toContain("reviewOffersAccept");
    expect(source).not.toContain("showTitleSuggestion");
    expect(source).not.toContain("showDescriptionSuggestion");
    // No `{#if` of any kind stands between "a suggestion is ready" and the
    // button that accepts it.
    expect(source).toMatch(/\{#if proposal\.state === "ready"\}(?:(?!\{#if)[\s\S])*?enhancements\.accept/);
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

  it("says why acceptance is refused instead of only greying the button out", () => {
    expect(source).toContain("const acceptBlock = $derived(");
    expect(source).toContain("Save or reload your edits before accepting a suggestion.");
    expect(source).toContain("This suggestion is for an older task revision. Ask again to apply it.");
    expect(source).toMatch(/\{#if !applyBlock && acceptBlock && proposal\.state === "ready"\}/);
    // The refusal and its reason are one expression, so the button can never be
    // disabled with nothing beside it — nor enabled while the sentence shows.
    expect(source).toContain("disabled={Boolean(acceptBlock) || disabled || controlsLocked || !selected.length}");
  });

  it("choosing which fields to accept is not an edit to the task", () => {
    /*
     * The sheet marks the task dirty from any `input` or `change` anywhere
     * inside its form, and this checkbox lives inside it. Left to bubble,
     * ticking a field marked the draft unsaved — and the unsaved-edits guard
     * then refused the very acceptance the checkbox was selecting.
     *
     * Both events, because a checkbox click fires both. Stopping only `change`
     * left `input` to mark it dirty and looked fixed while nothing had changed.
     */
    // One contiguous string, so both handlers are provably on the same element.
    expect(source).toContain(
      'oninput={(event) => event.stopPropagation()} onchange={(event) => { event.stopPropagation(); toggleSelected(',
    );
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
