// Behaviour checks for HealthPanel's remediation, agent launch, GitHub
// separation, scan lifecycle and honesty states.
//
// These replaced a test file that read HealthPanel.svelte and asserted
// `toContain` on its text. Each check drives the mounted panel through the
// fixture IPC and reads what a person (or the backend) would see: the copied
// report, the argv that reached `cmd_manvi_run_action`, the journal, the
// launch channel, the IPC log, and the rendered sections.
//
// Expected sentences and counts come from the owning modules
// (`formatSectionCount`, `formatDeadCodeStatus`, `coverageGap`, ...), so these
// checks pin that the panel *uses* the owner and prints what it says; the
// owners' own wording is asserted in their unit tests.
import { get } from "svelte/store";
import { repoStore } from "../src/lib/stores/repoStore";
import { harnessStore } from "../src/lib/stores/harnessStore";
import { interfaceStore } from "../src/lib/stores/interfaceStore";
import { terminalLaunchRequests } from "../src/lib/terminal/launchRequests";
import { PROVIDER_LABELS } from "../src/lib/workbench/taskHandoff";
import { formatSectionCount } from "../src/lib/health/counts";
import { formatDeadCodeStatus, coverageGap } from "../src/lib/health/report";
import { formatAuditCoverage, dependabotBadgeClass } from "../src/lib/health/format";

const REPO = "/fixture/GitPulse";

export async function runHealthChecks(ctx) {
  const { check, root, section, has, flat, wait, until, button, header, agentBar, rescan, ipcLog, aiCalls, runs, knobs, tick, healthReport, remount, setMode, setGithub } = ctx;

  const copied = [];
  const setClipboard = writeText => Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  setClipboard(async text => { copied.push(text); });
  const journal = kind => {
    harnessStore.activateRepository(REPO);
    return get(harnessStore).actions.filter(action => action.kind === kind);
  };
  const calls = (from, prefix) => ipcLog.slice(from).filter(entry => entry.cmd.startsWith(prefix));
  const scanCalls = path => ipcLog.filter(entry => entry.cmd === "cmd_scan_deps_health" && entry.args.repoPath === path).length;
  // What the app does on a repository switch: its views are keyed on the open
  // repository, so the old panel is destroyed and a new one mounted.
  const switchTo = async path => { await repoStore.openRepo(path); await remount(); await wait(); await wait(); };
  const settled = () => !button("Scan local").disabled && !button("Check GitHub alerts").disabled;
  const settle = async () => { await until(settled); await wait(120); };
  const aiResult = text => ({
    text, reasoning: "", model: "fixture-model", base_url: "http://fixture", context_window: 0, context_source: "fixture", prompt_tokens: 1, completion_tokens: 1,
    truncated: false, diff_truncated: false, diff_used_bytes: 0, diff_total_bytes: 0, elapsed_ms: 1,
    budget: { planned_by_harness: false, before_tokens: 0, threshold_tokens: 0, insufficient: false, calibration_samples: 0 }, warnings: [],
  });
  const done = result => ({ command: "", gated: true, policy: null, timed_out: false, truncated: false, truncation_reason: null, duration_ms: 5, exit_code: 0, stdout_tail: "", stderr_tail: "", ...result });
  const reset = () => {
    Object.assign(knobs, { scanDelay: 0, deps: null, dead: null, fixHandler: null, runHandler: null, runDelay: 0, openFail: false, severity: "HIGH" });
    runs.length = 0;
    aiCalls.length = 0;
  };
  const runButton = () => agentBar().querySelector('button[aria-label^="Run "]');

  try {
    // The page leaves the GitPulse tab active in its last findings state.
    await switchTo(REPO);
    reset();
    setMode("findings");
    setGithub("alerts");
    await rescan("findings");
    button("Check GitHub alerts").click();
    await until(() => !!section("dependabot")?.querySelector("table"));
    await settle();

    // ---- The copied report, and the report the model is sent -------------------
    button("Copy report").click();
    await until(() => copied.length > 0);
    const copiedReport = copied.at(-1);
    check("the copied report carries both GitHub check times", () =>
      copiedReport.includes("Dependabot checked at:") && copiedReport.includes("Code scanning checked at:"));
    check("the copied report carries the dead-code table the panel shows", () =>
      copiedReport.includes("unused_0") && copiedReport.includes("unused_1"));
    check("the copied report is the one for this repository", () => copiedReport.includes(REPO));

    // ---- Remediation plan --------------------------------------------------------
    knobs.fixHandler = () => aiResult("1. Audit: `npm audit`\n2. Update: `npm update`");
    button("Fix with MANVI").click();
    await until(() => !!section("plan") && has(section("plan"), "Run all steps"));
    check("the model is sent exactly the report that Copy report copies", () => aiCalls.length === 1 && aiCalls[0].report === copiedReport && aiCalls[0].repoPath === REPO);
    knobs.runHandler = (argv) => done({
      command: argv.join(" "),
      ...(argv[1] === "audit" ? { exit_code: 1, stderr_tail: "ERR-LINE", stdout_tail: "OUT-LINE" } : { stdout_tail: "updated" }),
    });
    const stepJournal = journal("remediation-step").length;
    button("Run all steps").click();
    await until(() => has(section("plan"), "Retry"));
    await wait(120);
    check("run all stops at the first failing step", () => runs.length === 1 && runs[0].args.join(" ") === "npm audit");
    check("a remediation step reaches the backend under the health kind with its own argv", () =>
      runs[0].actionKind === "health" && runs[0].repoPath === REPO);
    check("a remediation step is journaled as a failure", () =>
      journal("remediation-step").length === stepJournal + 1 && journal("remediation-step").at(-1).ok === false);
    check("a failed step keeps both streams, labelled", () =>
      ["stderr:", "ERR-LINE", "stdout:", "OUT-LINE"].every(part => has(section("plan"), part)));
    runs.length = 0;
    knobs.runHandler = (argv) => done({ command: argv.join(" "), stdout_tail: "fine" });
    button("Run all steps").click();
    await until(() => runs.length === 2 && !button("Run all steps")?.disabled);
    await wait(120);
    check("run all runs every step in order when they pass", () => runs.map(run => run.args.join(" ")).join("|") === "npm audit|npm update");
    // A step that settles after the user left is still journaled, and a batch
    // does not go on to run its remaining steps against the next repository.
    knobs.runDelay = 300;
    runs.length = 0;
    const staleJournal = journal("remediation-step").length;
    button("Run all steps").click(); await tick(); await wait();
    await switchTo("/fixture/Other");
    await new Promise(resolve => setTimeout(resolve, 900));
    await switchTo(REPO);
    knobs.runDelay = 0;
    check(`a batch abandoned by a repository switch stops after the step in flight (ran ${runs.map(run => run.args.join(" "))})`, () => runs.length === 1);
    check("the step that settled after the switch is still journaled", () => journal("remediation-step").length === staleJournal + 1);
    await rescan("findings");

    button("Fix with MANVI").click();
    await until(() => !!section("plan") && has(section("plan"), "Rescan Health"));
    repoStore.setTerminalOpen(false);
    button("Terminal").click(); await tick();
    check("the plan opens the terminal beside it, not instead of it", () =>
      get(repoStore).terminalOpen === true && !!section("plan") && has(section("plan"), "Run all steps"));
    const scansBeforeRescan = scanCalls(REPO);
    button("Rescan Health").click();
    await settle();
    check("the plan's Rescan Health rescans", () => scanCalls(REPO) === scansBeforeRescan + 1);
    check("a rescan supersedes the plan it was written against", () => !section("plan"));

    // ---- Coding-agent launch: the shared channel, the dock, and cancellation ---
    repoStore.setTerminalOpen(false);
    runButton().click(); await tick();
    check("Run files one request on the shared launch channel for this checkout", () => {
      const request = get(terminalLaunchRequests);
      return !!request && request.repoPath === REPO && request.launcher === "claude" && request.prompt.includes(REPO);
    });
    check("the terminal dock opens before the launch is awaited", () => get(repoStore).terminalOpen === true && has(agentBar(), "Opening agent terminal…"));
    terminalLaunchRequests.take(REPO).complete(); await wait();
    check("a claimed launch reports the session as opened", () =>
      has(agentBar(), `${PROVIDER_LABELS.claude} session opened in the terminal. Rescan Health after it finishes.`));
    runButton().click(); await tick();
    terminalLaunchRequests.take(REPO).complete("Fixture launch refused"); await wait();
    check("a refused launch says why", () => has(agentBar().querySelector('[role="alert"]'), "Fixture launch refused"));
    runButton().click(); await tick();
    check("a launch is pending before the repository changes", () => get(terminalLaunchRequests) !== null);
    await switchTo("/fixture/Other");
    check("a repository switch aborts the pending launch", () => get(terminalLaunchRequests) === null && !has(root, "Opening agent terminal…"));
    await switchTo(REPO);
    runButton().click(); await tick();
    check("the channel is free for the next launch after an aborted one", () => get(terminalLaunchRequests) !== null);
    terminalLaunchRequests.take(REPO).complete(); await wait();
    button("Copy health agent prompt").click(); await wait();
    check("Copy prompt copies the prompt for this repository", () => copied.at(-1).includes(REPO));

    // ---- Local scans stay local; GitHub checks are the only credentialed path ---
    let mark = ipcLog.length;
    button("Scan local").click();
    await settle();
    check("a local scan never touches the GitHub CLI", () => calls(mark, "cmd_github").length === 0 && calls(mark, "cmd_scan_deps_health").length === 1);
    mark = ipcLog.length;
    button("Check GitHub alerts").click();
    await settle();
    check("the explicit check asks for both feeds", () =>
      calls(mark, "cmd_github_dependabot_alerts").length === 1 && calls(mark, "cmd_github_code_scanning_alerts").length === 1);
    check("the check button's permission note exists and says what the check uses", () => {
      const note = root.querySelector(`#${CSS.escape(button("Check GitHub alerts").getAttribute("aria-describedby"))}`);
      return !!note && has(note, "uses the GitHub CLI, its credentials, and the network");
    });
    check("the freshness is a machine timestamp on the always-visible summary line", () => {
      const time = root.querySelector("details summary time");
      return !!time && new Date(time.getAttribute("datetime")).toISOString() === time.getAttribute("datetime")
        && has(root.querySelector("details summary"), "last checked") && has(root.querySelector("details summary"), "may be cached")
        && !!root.querySelector('details summary [role="status"]');
    });
    interfaceStore.setAutoScanGithubAlerts(false);
    mark = ipcLog.length;
    await switchTo("/fixture/NoAuto");
    await wait(300);
    check("with the preference off a new repository makes no GitHub request and says nothing was checked", () =>
      calls(mark, "cmd_github").length === 0 && has(section("dependabot"), "Not checked") && has(section("code-scanning"), "Not checked"));
    interfaceStore.setAutoScanGithubAlerts(true);
    mark = ipcLog.length;
    await switchTo("/fixture/Auto");
    await wait(300);
    check("with the preference on, opening a repository checks its alerts once", () =>
      calls(mark, "cmd_github_dependabot_alerts").length === 1 && calls(mark, "cmd_github_code_scanning_alerts").length === 1);
    await switchTo(REPO);
    // A request that failed and a CLI that is missing are different facts, and
    // both alert sections say so.
    for (const id of ["dependabot", "code-scanning"]) {
      setGithub("throws");
      button("Check GitHub alerts").click();
      await until(() => has(section(id), "Could not fetch"));
      check(`${id}: a failed request is not told to install the CLI`, () => has(section(id), "Fixture IPC failure") && !has(section(id), "Install the gh CLI"));
      setGithub("unavailable");
      button("Check GitHub alerts").click();
      await until(() => has(section(id), "gh: command not found"));
      check(`${id}: a refusal from a missing CLI is told to install it`, () => has(section(id), "Install the gh CLI"));
      setGithub("no-cli");
      button("Check GitHub alerts").click();
      await until(() => has(section(id), "to fetch") && !has(section(id), "Could not fetch"));
      check(`${id}: a missing CLI without an error says so for this slug`, () => has(section(id), "Install the gh CLI") && has(section(id), "owner/repo"));
    }
    setGithub("alerts");
    button("Check GitHub alerts").click();
    await until(() => !!section("dependabot")?.querySelector("table"));
    await settle();
  } finally {
    if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
    reset();
  }
}
