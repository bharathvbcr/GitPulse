import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const read = (name: string) => readFileSync(new URL(`./${name}.svelte`, import.meta.url), "utf8");
const form = read("TaskHandoffForm");
const sheet = read("TaskHandoffSheet");
const panel = read("TaskAgentPanel");
const owner = readFileSync(new URL("../workbench/taskTerminal.ts", import.meta.url), "utf8");

describe("the agent handoff has one implementation", () => {
  it("compiles all three without warnings", () => {
    for (const [name, source] of [["TaskHandoffForm", form], ["TaskHandoffSheet", sheet], ["TaskAgentPanel", panel]] as const) {
      const { warnings } = compile(source, { generate: "client", filename: `${name}.svelte` });
      expect(warnings.filter((w) => w.code !== "css-unused-selector"), name).toEqual([]);
    }
  });

  it("offers Grok and Antigravity from the same provider list as Codex and Claude Code", () => {
    expect(form).toContain("PROVIDER_CHOICES");
    expect(form).toContain("PROVIDER_LABELS[provider]");
    expect(form).not.toContain('choose({ provider: "codex" })');
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

  it("leaves worktree creation to the host and terminal opening to one owner", () => {
    // The form used to create the worktree itself, before the store accepted
    // anything, and never removed it: each refused attempt leaked a worktree
    // and a branch. The host now owns both creation and rollback.
    expect(form).not.toContain("cmd_add_worktree");
    expect(form).toContain("worktree: true");
    // Both places that start or show a task terminal share one implementation,
    // which queues before opening so a superseded open cannot lose the
    // terminal. The form only ever STARTS one: a launch must never take the
    // reader off the sheet that launched it. Showing is the pane's button.
    // The form hands an accepted attempt to the module owner, which starts
    // (never shows) its terminal; showing is the pane's button.
    expect(form).toContain("startPreparedAttempt(run, { remember: chosen })");
    expect(form).not.toContain("startTaskTerminal(");
    expect(form).not.toContain("showTaskTerminal(");
    expect(owner).toContain("startTerminal: startTaskTerminal,");
    expect(owner.slice(owner.indexOf("export async function startPreparedAttempt"))).not.toMatch(/await (deps\.show|showTaskTerminal|showAttemptTerminal)\(/);
    expect(panel).toContain("showAttemptTerminal(run)");
    for (const [name, host] of [["TaskHandoffForm", form], ["TaskAgentPanel", panel], ["TaskHandoffSheet", sheet]] as const) {
      expect(host, name).not.toContain("enqueueTaskTerminal(");
      expect(host, name).not.toContain("repoStore.openRepo(");
      expect(host, name).not.toContain("setGlobalSurface(");
      expect(host, name).not.toContain("setTerminalOpen(");
    }
    // A checkout another agent holds forces the worktree, visibly.
    expect(form).toContain("const useWorktree = $derived(provisionWorktree || occupant !== null)");
    expect(form).toContain('cause.code === "checkout_busy"');
  });

  it("describes permission modes with the one table Settings and the terminal use", () => {
    // The form and the pane each carried their own wording ("Allow workspace
    // edits"), which disagreed with Settings → Agents ("Edit files") for the
    // same mode. One table, and its detail line under the choice.
    for (const [name, host] of [["TaskHandoffForm", form], ["TaskAgentPanel", panel]] as const) {
      expect(host, name).toContain('import { PERMISSION_LABELS } from "../terminal/agentDefaults"');
      expect(host, name).not.toMatch(/const PERMISSION_LABELS\s*:/);
    }
    expect(form).toContain('data-testid="permission-detail"');
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
    expect(launch.indexOf("onPrepared?.(run)")).toBeLessThan(launch.indexOf("await starting"));
    // An accepted attempt is started before anything asks whether this form
    // still exists: closing the sheet or switching task tabs must not
    // abandon it (it used to `if (disposed) return` right here).
    expect(launch.indexOf("startPreparedAttempt(run")).toBeGreaterThan(-1);
    expect(launch.indexOf("startPreparedAttempt(run")).toBeLessThan(launch.indexOf("disposed"));
    expect(form).not.toContain("launchManagedRun");
    // Both signals reach the panel, which dedups on run id.
    expect(panel).toContain("onPrepared={prepared}");
    expect(panel).toContain("onLaunched={launched}");
    expect(panel).toContain("runs.filter((item) => item.id !== run.id)");
    // The sheet stops offering a launch once one exists, so a failure after
    // preparation cannot be answered by making a second attempt.
    expect(sheet).toContain("{#if prepared && !busy}");
    expect(sheet).toContain("Resume or cancel it from this task's Agents tab.");
  });

  it("never lets bypass survive the attempt it was authorized for", () => {
    expect(form).toContain('if (settings.permission === "bypass") settings = { ...settings, permission: defaultHandoff().permission };');
    expect(form).toContain("acknowledged = false;");
    // Remembering belongs to the owner, which downgrades bypass before it
    // stores anything — even before `sanitizeHandoff` refuses to restore it.
    expect(form).not.toContain("setTaskHandoff");
    expect(owner).toContain('deps.remember(settings.permission === "bypass" ? { ...settings, permission: defaultHandoff().permission } : settings);');
  });

  it("shows the run before the form once one is live", () => {
    // "Live" has to mean the same thing here as it does to the store, which
    // releases a prepared attempt the moment it expires. While this filter
    // looked only at `state`, an expired preparation kept the form folded
    // away — so the one control that could recover the situation was hidden
    // behind a row that was already dead.
    // The live rows are the monitored ones, and only runs that still hold
    // their checkout are monitored; history is everything else.
    expect(panel).toContain("const rows = allRuns.filter((run) => runHoldsCheckout(run, clock)).map(monitor);");
    expect(panel).toContain("const live = $derived(monitored.map((row) => row.run));");
    expect(panel).toContain("const ended = $derived(allRuns.filter((run) => !runHoldsCheckout(run, clock))");
    expect(panel).not.toMatch(/LIVE\.includes/);
    expect(panel).toContain("const formOpen = $derived(expandedForm ?? live.length === 0)");
    // Tailwind preflight's `[hidden]` has zero specificity; without this the
    // fold would be decorative and the form would stay on screen.
    expect(panel).toContain("#task-agent-launch[hidden]{display:none}");
  });

  it("keeps polling scoped to runs that can still change", () => {
    expect(panel).toContain('["starting", "running"].includes(run.state)');
    expect(panel).toContain("run.expires_at * 1000 > clock");
    // The working agents are read apart from the history page, so an old one
    // can be on the live list alone. Polling read only the history page, and
    // stopped while that agent was still running.
    expect(panel).toContain("live: allRuns.some((run) =>");
    expect(panel).not.toContain("live: runs.some(");
    expect(panel).toContain("readBackgroundDocument");
    expect(panel).toContain("nextTaskRunPollDelay");
    expect(panel).toContain("if (!active)");
    expect(panel).toContain("runs.length >= MAX_LOADED_RUNS");
  });

  it("answers 'keep polling' and 'still live' from one instant", () => {
    // These two decisions used to read different clocks — the poll loop called
    // `Date.now()` inline while the liveness filter had no clock at all — so
    // the panel could stop polling a row and go on rendering it as live. The
    // shared `clock` is advanced where polling is reconsidered, which is also
    // the last tick after an expiry, and that tick is what flips the row.
    expect(panel).toContain("let clock = $state(Date.now());");
    expect(panel).toContain("clock = Date.now();");
    // Nothing may reach for the wall clock again behind `clock`'s back.
    const body = panel.slice(panel.indexOf("function schedule()"));
    expect(body.slice(0, body.indexOf("async function refresh"))).not.toMatch(/expires_at[^\n]*Date\.now\(\)/);
  });
});
