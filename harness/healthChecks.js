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
  const { check, root, section, has, flat, wait, until, button, header, agentBar, rescan, ipcLog, aiCalls, runs, knobs, tick, healthReport, remount, detach, attach, setMode, setGithub } = ctx;

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
  const switchTo = async path => { await detach(); await repoStore.openRepo(path); await attach(); await wait(); await wait(); };
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
  const noScans = () => ipcLog.filter(entry => ["cmd_scan_coverage", "cmd_scan_secrets", "cmd_storage_scan"].includes(entry.cmd)).length === 0;
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
    // A host that keeps the panel mounted: the panel itself aborts on a switch.
    runButton().click(); await tick();
    await repoStore.openRepo("/fixture/Other"); await wait();
    check("an unkeyed panel aborts the pending launch itself on a repository switch", () => get(terminalLaunchRequests) === null && !has(root, "Opening agent terminal…"));
    await switchTo(REPO);
    runButton().click(); await tick();
    check("the channel is free for the next launch after an aborted one", () => get(terminalLaunchRequests) !== null);
    terminalLaunchRequests.take(REPO).complete(); await wait();
    button("Copy health agent prompt").click(); await wait();
    check("Copy prompt copies the prompt for this repository", () => copied.at(-1).includes(REPO));
    await until(() => !!button("Copy health agent prompt"));
    const execBefore = document.execCommand;
    setClipboard(async () => { throw new Error("fixture denied"); });
    document.execCommand = () => false;
    button("Copy health agent prompt").click(); await wait();
    check("a refused prompt copy says so and opens the preview to copy from", () =>
      has(agentBar(), "Could not copy. Open Preview prompt and copy it from there.") && !!agentBar().querySelector('button[aria-expanded="true"]'));
    document.execCommand = execBefore;
    setClipboard(async text => { copied.push(text); });

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

    // ---- Scan lifecycle: one scan per visit, cache, no foreign data -------------
    const visits = scanCalls("/fixture/Visit");
    await switchTo("/fixture/Visit");
    await wait(400);
    check("opening a repository scans it exactly once", () => scanCalls("/fixture/Visit") === visits + 1, () => scanCalls("/fixture/Visit") - visits);
    knobs.scanDelay = 700;
    await switchTo("/fixture/Other");
    await switchTo(REPO);
    check("a revisit shows the cached report while the fresh scan runs", () => !!section("summary") && button("Scan local").disabled);
    await settle();
    await switchTo("/fixture/Fresh");
    check("a repository with nothing cached shows none of the previous repository's findings", () =>
      !section("summary") && !section("vulnerabilities") && !has(root, "unused_0"));
    await settle();
    check("and shows its own report once its scan lands", () => !!section("summary"));
    // The same switch without a remount, for a host that keeps the panel mounted:
    // the panel clears the previous repository's data itself.
    knobs.scanDelay = 700;
    await repoStore.openRepo("/fixture/Fresh3"); await wait(100);
    check("an unkeyed panel clears the previous repository's data while the new one scans", () =>
      !section("summary") && !has(root, "unused_0"));
    await settle();
    knobs.scanDelay = 0;
    await switchTo(REPO);
    await settle();
    knobs.scanDelay = 700;
    button("Scan local").click(); await tick(); await wait();
    check("a rescan keeps the report on screen while it runs", () => !!section("summary") && button("Scan local").disabled);
    await settle();
    knobs.scanDelay = 0;

    // ---- Failure separation -----------------------------------------------------
    knobs.openFail = true;
    button("Open the advisory for pkg-1").click();
    await until(() => has(root, "Fixture opener refused"));
    check("a link that fails to open is reported without replacing the report", () => !!section("summary") && !!section("vulnerabilities"));
    knobs.openFail = false;
    button("Scan local").click();
    await settle();
    check("a new scan clears the stale action error", () => !has(root, "Fixture opener refused"));
    check("Dependabot severities of any casing tint the header badge by the shared normalizer", () => {
      const badge = [...header().querySelectorAll("span")].find(node => node.textContent.includes("Dependabot"));
      return !!badge && badge.className.includes(dependabotBadgeClass([{ severity: "high" }]));
    });
    knobs.severity = "Critical";
    button("Check GitHub alerts").click(); await settle();
    check("a mixed-case critical alert tints like critical", () => {
      const badge = [...header().querySelectorAll("span")].find(node => node.textContent.includes("Dependabot"));
      return !!badge && badge.className.includes(dependabotBadgeClass([{ severity: "critical" }]));
    });
    knobs.severity = "MODERATE";
    button("Check GitHub alerts").click(); await settle();
    check("a moderate alert tints differently from a high one", () => {
      const badge = [...header().querySelectorAll("span")].find(node => node.textContent.includes("Dependabot"));
      return !!badge && badge.className.includes(dependabotBadgeClass([{ severity: "moderate" }])) && dependabotBadgeClass([{ severity: "moderate" }]) !== dependabotBadgeClass([{ severity: "high" }]);
    });
    knobs.severity = "HIGH";
    button("Check GitHub alerts").click(); await settle();
    setMode("malformed");
    button("Scan local").click(); await settle();
    check("a payload the parser refuses is reported, and the panel is usable again", () => has(root, "omitted scanner presence") && !button("Scan local").disabled);
    await rescan("findings");

    // ---- Dead code: each answer is told apart -----------------------------------
    const deadAnswer = over => () => ({ source_freshness: { fresh: true, generation_id: 12 }, available: true, reason: null, items: [], total: 0, shown: 0, truncated: false, rungs: { deterministic: 0, high: 0, speculative: 0, filtered_out: 0 }, ...over });
    knobs.dead = deadAnswer({ available: false, reason: "index is stale" });
    await rescan("findings");
    check("a dead-code check that could not run is not read as finding nothing", () =>
      has(section("dead-code"), "Dead-code check could not run: index is stale") && has(section("dead-code"), "not the same as finding no unreferenced symbols"));
    knobs.dead = deadAnswer({});
    await rescan("findings");
    check("an answered, empty graph says there are no candidates", () => has(section("dead-code"), "No dead-code candidates in the indexed graph."));
    knobs.dead = deadAnswer({ truncated: true, total: 5 });
    await rescan("findings");
    check("an empty answer from a truncated query is not an all-clear", () => has(section("dead-code"), "stopped at its token budget before returning anything; this is not an all-clear."));
    knobs.dead = null;
    await rescan("findings");
    check("the dead-code heading counts the observed total with the truncation marker", () =>
      has(section("dead-code").querySelector("h3"), formatSectionCount({ shown: 2, total: 14, atLeast: true })));
    check("exempt rows carry their reason and the others the shared status wording", () => {
      const cells = [...section("dead-code").querySelectorAll("tbody tr td:last-child")].map(flat);
      return cells.includes(formatDeadCodeStatus({ is_exempt: true, exemption_reason: "public API" }))
        && cells.includes(formatDeadCodeStatus({ is_exempt: false, exemption_reason: null }));
    });

    // ---- Counts: observed totals, never the surviving rows ------------------------
    await rescan("huge");
    const heading = id => flat(section(id).querySelector("h3"));
    check("the vulnerability heading carries the observed total and what it shows", () =>
      heading("vulnerabilities").includes(formatSectionCount({ shown: 400, total: 1200 })));
    root.querySelector('[aria-label="Vulnerability scope"] button:nth-child(2)').click(); await tick();
    check("a capped scan's direct count is a floor, never a total", () =>
      heading("vulnerabilities").includes(formatSectionCount({ shown: 134, total: 134, atLeast: true, qualifier: "direct" })), () => heading("vulnerabilities"));
    root.querySelector('[aria-label="Vulnerability scope"] button:nth-child(1)').click(); await tick();
    check("the outdated heading counts the observed total", () => heading("outdated").includes(formatSectionCount({ shown: 400, total: 1200 })));
    await rescan("hostile");
    button("Copy report").click(); await until(() => copied.at(-1)?.includes("Repository: "));
    check("the copied report carries an incomplete dead-code walk", () => copied.at(-1).includes("170746 of 331397"));
    await rescan("findings");
    check("the outdated section is headed as npm-only", () => has(section("outdated").querySelector("h3"), "Outdated npm packages"));
    check("the target census is worded by its owner in the vulnerabilities section", () =>
      flat(section("vulnerabilities").querySelector('[data-testid="audit-coverage"]')) === `${formatAuditCoverage({ found: 2, audited: 1, unaudited: ["cargo:Cargo.lock"] })}.`);
    check("an ecosystem is a definition list that says how many artifacts it is not showing", () => {
      const entries = [...section("packages").querySelectorAll("dl dd ul li")].map(flat);
      return flat(section("packages").querySelector("dl dt")) === "cargo" && entries.length === 5 && entries[4] === "+32 more" && entries.every(entry => !entry.includes(","));
    });
    const failing = () => {
      const report = healthReport();
      return { ...report, scanners_ran: ["npm"], audit_complete: false, vulnerabilities: [], audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, unknown: 0, total: 0 },
        issues: [{ severity: "error", code: "audit_failed", message: "npm audit exited 1", path: "package.json" }] };
    };
    knobs.deps = failing;
    await rescan("findings");
    check("a scanner that ran and failed is named, not only one that is missing", () =>
      has(section("vulnerabilities"), `Local audit incomplete (${coverageGap(failing())}); no all-clear is available.`) && coverageGap(failing()).includes("failed: npm audit"));
    knobs.deps = null;
    await rescan("findings");
    check("Health never scans coverage, secrets or storage itself", noScans);
  } finally {
    if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
    reset();
  }
}
