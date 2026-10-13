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
    expect(form).toContain('const useWorktree = $derived(provisionWorktree || holder.kind !== "free")');
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

  it("offers a launch's model as a choice where the launcher has one, and as text otherwise", () => {
    for (const field of ["model", "advisor"]) {
      expect(form).toContain(`modelOverrideChoices(settings.provider, "${field}")`);
    }
    expect(form).toMatch(/<select class="gp-select" aria-label="Model"[\s\S]*?\{#each choices as id \(id\)\}/);
    expect(form).toMatch(/<select class="gp-select" aria-label="Advisor model"[\s\S]*?\{#each choices as id \(id\)\}/);
    expect(form).toMatch(/\{:else\}\s*<input class="gp-field" aria-label="Model"/);
  });

  it("sends the sheet's key and button through one guard that refuses after a preparation", () => {
    // Once the store accepted an attempt whose start then failed, the footer
    // swapped Launch for Done — but Cmd+Enter still called the form, which
    // passed its gate again and prepared a second run in a forced worktree.
    expect(sheet).toMatch(/function launch\(\)\s*\{\s*if \(prepared \|\| busy \|\| !gate\.ok\) return;/);
    expect(sheet.match(/form\?\.launch\(\)/g)).toHaveLength(1);
    expect(sheet).toContain('event.key === "Enter") { event.preventDefault(); launch(); }');
    expect(sheet).toContain("onclick={launch}");
  });

  it("lets the sheet close once the attempt is accepted, while its start is still running", () => {
    // The start belongs to `startPreparedAttempt` and outlives the sheet, but
    // `busy` stays set through it, and the terminal's start awaits repository
    // reads with no bound. Close, Escape and Done must not wait on that.
    expect(sheet).toContain("const closable = $derived(!busy || prepared);");
    expect(sheet).toContain('event.key === "Escape" && closable)');
    expect(sheet).toContain('aria-label="Close agent handoff" disabled={!closable}');
    expect(sheet).toMatch(/\{#if prepared\}\s*<span class="meta gate">/);
    expect(sheet).not.toContain("{#if prepared && !busy}");
  });

  it("keeps an unanswered preparation for its task, past the form that sent it", () => {
    // Held in the form, it died with the sheet or the task tab, and the next
    // press minted a new identity for what may already be a committed run.
    expect(form).toContain("let pending = $state<RunPreparation | null>(untrack(() => pendingPreparationFor(taskId)));");
    const launch = form.slice(form.indexOf("export async function launch()"));
    // Held as the plain object, before it becomes state: `$state` deep-proxies
    // it, and structuredClone throws on a proxy, which failed every launch.
    expect(launch.indexOf("holdPendingPreparation(prepared);\n        pending = prepared;")).toBeGreaterThan(-1);
    expect(launch.indexOf("holdPendingPreparation(prepared)")).toBeLessThan(launch.indexOf("prepareTaskRun(preparation)"));
    expect(launch).not.toContain("holdPendingPreparation(pending)");
    // A restored copy lets go once another form settles the hold.
    expect(form).toMatch(/\$pendingPreparationsChanged;\s*untrack\(\(\) => \{\s*if \(!pending \|\| busy \|\| pendingPreparationFor\(pending\.task_id\)\?\.id === pending\.id\) return;\s*pending = null;/);
    expect(launch).toContain("releasePendingPreparation(preparation.task_id, preparation.id);\n      pending = null;");
    // A settled refusal is released even when it lands after the form is gone.
    const handler = launch.slice(launch.indexOf("} catch (cause) {"));
    expect(handler.indexOf("releasePendingPreparation(sent.task_id, sent.id)")).toBeGreaterThan(-1);
    expect(handler.indexOf("releasePendingPreparation(sent.task_id, sent.id)")).toBeLessThan(handler.indexOf("if (disposed) return;"));
    // The way out is deliberate and says what it cannot know.
    expect(form).toContain('onclick={discard}>Discard it</button>');
    expect(form).toMatch(/function discard\(\) \{\s*if \(!pending \|\| busy\) return;\s*releasePendingPreparation\(pending\.task_id, pending\.id\);/);
  });

  it("says the store answered when its reply to a preparation could not be read", () => {
    expect(form).toContain('cause.code === "protocol_error"');
    expect(form).toContain("this attempt may exist: check this task's Agents before launching again.");
  });

  it("asks whether a checkout is taken by its Git directory, without ever waiting on the answer", () => {
    // The store refuses by `git_dir`; an agent in a subfolder of the chosen
    // checkout was invisible to a comparison of directories.
    expect(form).toContain("checkoutHolder(holding, holdingError, { path: checkout, git_dir: targetGitDir }, pathOpts)");
    const ask = form.slice(form.indexOf("let identityTicket"), form.indexOf("let identityTicket") + 900);
    // Debounced, bounded, ticketed, and a failure is only "unknown".
    expect(ask).toContain("setTimeout(");
    expect(ask).toContain("return () => clearTimeout(timer);");
    expect(ask).toContain("bounded(checkoutIdentity(path))");
    expect(ask).toContain("ticket === identityTicket");
    expect(ask).toMatch(/\(\) => \{\s*if \(!disposed && ticket === identityTicket\) targetGitDir = null;/);
    // Launch never waits on it: the gate does not read it.
    expect(form.slice(form.indexOf("const gate = $derived("), form.indexOf("const gate = $derived(") + 200)).not.toContain("targetGitDir");
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
    // One named exception: Discard is the way out of the lock, so it is
    // usable exactly while the lock holds, and refused only mid-request.
    const discard = '<button type="button" class="gp-btn discard" disabled={busy} onclick={discard}>Discard it</button>';
    expect(form).toContain(discard);
    const markup = form.slice(form.indexOf("</script>")).replace(discard, "");
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
    // From the moment it exists — not only after its start has finished.
    expect(sheet).toMatch(/\{#if prepared\}\s*<span class="meta gate">/);
    expect(sheet).toContain("Resume or cancel it from this task's Agents tab.");
  });

  it("never lets the owner remember bypass, whatever the form sends it", () => {
    // Rendered in harness/task-runs.html: "Bypass requires acknowledgment on
    // this attempt", "Bypass never survives the attempt it was authorized
    // for" (the form resets to the default and drops the acknowledgment), and
    // "The remembered handoff never carries bypass into storage"
    // (sanitizeHandoff). What only the source shows: the form does not store
    // the handoff itself, and the owner downgrades before it stores.
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

  it("keeps polling scoped to runs that still hold a checkout, by the shared helper", () => {
    // Rendered in harness/task-runs.html: "Hidden task inspector stops run
    // polling", "Visible inspector refreshes immediately after resume", and
    // "The pane goes on polling for it, and sees it end without a manual
    // refresh" (an agent older than a page of history). What the fixture
    // never serves is an `unresolved` attempt, which a hand-copied state
    // list here once dropped; the helper is what keeps it polled.
    expect(panel).toContain("live: allRuns.some((run) => runHoldsCheckout(run, clock)),");
    expect(panel).not.toMatch(/live: allRuns\.some\(\(run\) => \[/);
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
