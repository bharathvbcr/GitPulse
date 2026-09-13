import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const read = (name: string) => readFileSync(new URL(`./${name}.svelte`, import.meta.url), "utf8");
const form = read("TaskHandoffForm");
const sheet = read("TaskHandoffSheet");
const panel = read("TaskAgentPanel");

describe("the agent handoff has one implementation", () => {
  it("compiles all three without warnings", () => {
    for (const [name, source] of [["TaskHandoffForm", form], ["TaskHandoffSheet", sheet], ["TaskAgentPanel", panel]] as const) {
      const { warnings } = compile(source, { generate: "client", filename: `${name}.svelte` });
      expect(warnings.filter((w) => w.code !== "css-unused-selector"), name).toEqual([]);
    }
  });

  it("leaves the launch to the form, so neither host can grow a second one", () => {
    // The board sheet and the editor panel are chrome around one form. If
    // either started calling `prepareTaskRun` itself, the checkout resolution,
    // the revision re-read and the remembered settings would immediately be
    // two different behaviours wearing one name.
    expect(form).toContain("prepareTaskRun");
    for (const [name, host] of [["TaskHandoffSheet", sheet], ["TaskAgentPanel", panel]] as const) {
      expect(host, name).toContain("TaskHandoffForm");
      expect(host, name).not.toContain("prepareTaskRun");
      expect(host, name).not.toContain("checkoutCandidates");
      expect(host, name).not.toContain("handoffGate");
    }
  });

  it("re-reads the saved task and refuses a revision that moved", () => {
    expect(form).toContain("await bounded(getTask(taskId))");
    expect(form).toContain("if (latest.revision !== revision)");
  });

  it("keeps one preparation identity across a retry and locks the form while it is unknown", () => {
    expect(form).toContain("if (!pending)");
    expect(form).toContain("request_id: newID()");
    expect(form).toContain("const locked = $derived(busy || disabled || pending !== null)");
    // Every control obeys the lock — derived from the markup rather than
    // counted by hand, because a control added later is exactly the one that
    // would be missed, and one editable control makes "Retry preparation" a lie.
    const markup = form.slice(form.indexOf("</script>"));
    const gates = markup.match(/\sdisabled=\{[^}]*\}/g) ?? [];
    expect(gates.length).toBeGreaterThanOrEqual(9);
    expect(gates.filter((gate) => !gate.includes("locked"))).toEqual([]);
  });

  it("publishes a prepared run before anything that can fail afterwards", () => {
    const launch = form.slice(form.indexOf("const run = await bounded(prepareTaskRun"));
    expect(launch.indexOf("onPrepared?.(run)")).toBeGreaterThan(-1);
    expect(launch.indexOf("onPrepared?.(run)")).toBeLessThan(launch.indexOf("launchManagedRun"));
    expect(launch.indexOf("onPrepared?.(run)")).toBeLessThan(launch.indexOf("openTerminalFor"));
    // The panel treats both signals the same way; it dedups on run id.
    expect(panel).toContain("onPrepared={launched}");
    expect(panel).toContain("runs.filter((item) => item.id !== run.id)");
    // The sheet stops offering a launch once one exists, so a failure after
    // preparation cannot be answered by making a second attempt.
    expect(sheet).toContain("{#if prepared && !busy}");
    expect(sheet).toContain("Resume or cancel it from this task's Agent tab.");
  });

  it("never lets bypass survive the attempt it was authorized for", () => {
    expect(form).toContain('if (settings.permission === "bypass") settings = { ...settings, permission: defaultHandoff().permission };');
    expect(form).toContain("acknowledged = false;");
    // Remembering happens after the downgrade, so storage never sees bypass
    // even before `sanitizeHandoff` refuses to restore it.
    expect(form.indexOf('permission: defaultHandoff().permission')).toBeLessThan(form.indexOf("interfaceStore.setTaskHandoff(settings)"));
  });

  it("shows the run before the form once one is live", () => {
    expect(panel).toContain("const live = $derived(runs.filter((run) => LIVE.includes(run.state)))");
    expect(panel).toContain("const formOpen = $derived(expandedForm ?? live.length === 0)");
    // Tailwind preflight's `[hidden]` has zero specificity; without this the
    // fold would be decorative and the form would stay on screen.
    expect(panel).toContain("#task-agent-launch[hidden]{display:none}");
  });

  it("keeps polling scoped to runs that can still change", () => {
    expect(panel).toContain('["starting", "running"].includes(run.state)');
    expect(panel).toContain("run.expires_at * 1000 > Date.now()");
    expect(panel).toContain('document.visibilityState !== "hidden"');
    expect(panel).toContain("if (!active)");
    expect(panel).toContain("runs.length >= 180");
  });
});
