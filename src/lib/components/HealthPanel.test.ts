import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthPanel, { dependabotFreshness } from "./HealthPanel.svelte";
import { HEALTH_SECTIONS } from "../health/sections";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HealthPanel.svelte"),
  "utf8",
);

describe("HealthPanel source contracts & interactive remediation", () => {
  it("invokes the scoped Manvi runner for remediation steps", () => {
    // The result type is the shared declaration, not an inline copy. This
    // panel used to spell the whole wire shape out anonymously at the invoke
    // call, which is why a backend field rename could reach it as `undefined`
    // with nothing to catch it.
    expect(source).toContain("invoke<TerminalRunResult>");
    expect(source).not.toContain("invoke<{");
    expect(source).toContain('"cmd_manvi_run_action"');
    expect(source).toContain('actionKind: "health"');
    expect(source).toContain("args: step.argv,");
  });

  it("keeps both output streams, and marks a clipped tail as clipped", () => {
    // Regression: this panel built its detail as `stderr || stdout`, so a
    // non-empty stderr discarded stdout entirely, and it never noted
    // truncation at all — a clipped tail was shown as the whole log.
    expect(source).toContain("formatRunDetail(res)");
    expect(source).toContain("formatRunSummary(res)");
    expect(source).not.toContain("res.stderr_tail || res.stdout_tail");
    expect(source).not.toContain('"Timed out and was killed."');
  });

  it("extracts plan steps and tokenizes commands safely", () => {
    expect(source).toContain("buildRunnablePlanSteps(plan.text)");
  });

  it("journals step execution into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
    expect(source).toContain('kind: "remediation-step",');
  });

  it("journals a settled step before any stale-UI return", () => {
    const body = source.slice(
      source.indexOf("async function runStep"),
      source.indexOf("async function runAllSteps"),
    );
    const settled = body.indexOf("await invoke<TerminalRunResult>");
    const successJournal = body.indexOf("harnessStore.recordAction", settled);
    const successGuard = body.indexOf("if (!guard.isLive()) return false;", settled);
    expect(successJournal).toBeGreaterThan(settled);
    expect(successJournal).toBeLessThan(successGuard);

    const caught = body.indexOf("} catch", successGuard);
    const failureJournal = body.indexOf("harnessStore.recordAction", caught);
    const failureGuard = body.indexOf("if (!guard.isLive()) return false;", caught);
    expect(failureJournal).toBeGreaterThan(caught);
    expect(failureJournal).toBeLessThan(failureGuard);
  });

  it("provides single-step and sequential run-all execution", () => {
    expect(source).toContain("async function runStep");
    expect(source).toContain("async function runAllSteps");
  });

  it("uses one cancellation guard for a batch and ignores stale step results", () => {
    const stepBody = source.slice(
      source.indexOf("async function runStep"),
      source.indexOf("async function runAllSteps"),
    );
    const batchBody = source.slice(
      source.indexOf("async function runAllSteps"),
      source.indexOf("async function copyPlan"),
    );
    expect(source).toContain("function beginSteps(): AsyncGuard");
    expect(stepBody).toContain("guard: AsyncGuard");
    expect(stepBody).toContain("if (!guard.isLive()) return false;");
    expect(batchBody).toContain("const guard = beginSteps()");
    expect(batchBody).toContain("runStep(step, guard)");
  });

  it("opens the terminal beside the plan rather than navigating away from it", () => {
    // This used to switch to the Terminal *view*, which replaced the pane —
    // the remediation plan the user was about to run left the screen at the
    // moment they needed to read it. The dock opens under this panel instead.
    expect(source).toContain("repoStore.setTerminalOpen(true)");
    expect(source).not.toContain('setActiveTab("terminal")');
    expect(source).toContain("Rescan Health");
  });
});

describe("HealthPanel rendering", () => {
  it("renders the Health header and initial scan state", () => {
    const { body } = render(HealthPanel);
    expect(body).toContain("Health");
    expect(body).toContain("Scan");
    expect(body).toContain("GitHub alerts are checked when GitPulse launches");
    // With no repository open there is nothing to have checked, so the
    // Dependabot state chip stays out of the header entirely.
    expect(body).not.toContain("Dependabot not checked");
    expect(body).not.toContain("Code scanning not checked");
    expect(body).toContain("Open a repository to scan dependency health");
  });

  /**
   * The "an unrun audit never renders as clean" contract used to be a string
   * match on this file's `formatAuditCounts(report.audit, { complete, ran })`
   * call. The verdict has one owner now, so the guard lives where the decision
   * does: `summary.test.ts` asserts the *behaviour* — an unrun audit is tone
   * `unknown` with the value "did not run", a partial one is marked a floor —
   * over every way a facet can be left unestablished, which the string match
   * could not check at all.
   *
   * What remains here is that this panel reads that owner and nothing else.
   */
  it("derives its verdict from the shared summary rather than its own arithmetic", () => {
    expect(source).toContain('from "../health/summary"');
    expect(source).toContain("summarizeHealth({");
    expect(source).toContain("report: current,");
    expect(source).toContain("dependabot,");
    expect(source).toContain("codeScanning,");
    expect(source).toContain("codegraph,");
    // Still fed the two honesty inputs, now through the summary rather than a
    // second local formatter.
    expect(source).toContain("report?.audit_complete === true");
    expect(source).toContain("(report?.scanners_ran ?? []).length > 0");
    // The panel must not re-derive the counts sentence beside the owner.
    expect(source).not.toContain("formatAuditCounts(");
  });

  it("includes Dependabot in the copied report and shows every alert severity in the header", () => {
    expect(source).toContain(
      "formatHealthReport(current, repoPath, dependabot, codeScanning, deadCode)",
    );
    expect(source).toContain("Dependabot checked at:");
    expect(source).toContain("Code scanning checked at:");
    expect(source).toContain("{#if openDependabotCount > 0}");
    expect(source).toContain("{#if openCodeScanningCount > 0}");
  });

  it("feeds the dead-code table into the copied report, not only the on-screen table", () => {
    const body = source.slice(
      source.indexOf("function renderedReport"),
      source.indexOf("async function copyReport"),
    );
    expect(body).toContain("deadSymbolsAvailable");
    expect(body).toContain("deadSymbolsReason");
    expect(body).toContain("deadSymbolsTotal");
    expect(body).toContain("deadSymbolsTruncated");
    expect(body).toContain("items: deadSymbols");
    expect(source).toContain(
      "formatHealthReport(current, repoPath, dependabot, codeScanning, deadCode)",
    );
  });

  it("parses the health report before the pane reads .length", () => {
    const localScan = source.slice(
      source.indexOf("async function scan("),
      source.indexOf("async function scanDependabot"),
    );
    expect(localScan).toContain("parseDepsHealthReport");
    expect(localScan).toContain("finally");
    expect(localScan).toContain("loading = false");
  });

  it("keeps credentialed GitHub checks off the local scan path", () => {
    const localScan = source.slice(
      source.indexOf("async function scan("),
      source.indexOf("async function scanDependabot"),
    );
    expect(localScan).not.toContain("cmd_github_dependabot_alerts");
    expect(localScan).not.toContain("cmd_github_code_scanning_alerts");
    expect(localScan).not.toContain("loadGithubAlerts");
    expect(source).toContain("async function scanDependabot");
    expect(source).toContain("GitHub alerts are checked when GitPulse launches");
    expect(source).toMatch(/uses the GitHub CLI,\s+its credentials, and the network/);
    expect(source).toContain('aria-describedby="dependabot-permission-note"');
    expect(source).not.toContain('class="hidden xl:inline text-[10px] text-textMuted"');
    expect(source).toContain("Check GitHub alerts");
    expect(source).toContain("onclick={() => scanDependabot(undefined, { force: true })}");
    expect(source).toContain("loadGithubAlerts");
    expect(source).toContain("autoScanGithubAlerts");
  });

  /**
   * Both alert sections were eighty-line hand-written copies of one five-state
   * machine, and the branch-order contract was asserted twice — once per copy.
   * They had already been fixed in lockstep twice. There is one implementation
   * now (`health/GithubAlertsSection.svelte`), whose own test owns the branch
   * order; what this file has to guarantee is that both call sites reach it
   * and hand it the right `requestFailed`, which is the field that tells a
   * failed request apart from a missing CLI.
   */
  it("routes both alert sections through the one shared state machine", () => {
    expect(source).toContain('import GithubAlertsSection from "./health/GithubAlertsSection.svelte"');
    expect(source).toContain('id="dependabot"');
    expect(source).toContain('id="code-scanning"');
    expect(source).toContain("requestFailed={dependabotRequestFailed}");
    expect(source).toContain("requestFailed={codeScanningRequestFailed}");
    // The copies must be gone, not merely unused.
    expect(source).not.toContain("{#snippet dependabotSection()}");
    expect(source).not.toContain("{#snippet codeScanningSection()}");
    expect(source).toContain("dependabotRequestFailed = snapshot.dependabotRequestFailed;");
    expect(source).toContain("dependabotRequestFailed = false;");
    expect(source).toContain("codeScanningRequestFailed = snapshot.codeScanningRequestFailed;");
  });

  it("labels outdated results as npm-only, through the catalog heading", () => {
    // The heading text moved to the section catalog so the jump chip and the
    // heading cannot disagree; the npm-only qualifier travels with it.
    expect(HEALTH_SECTIONS.find((s) => s.id === "outdated")?.heading).toBe(
      "Outdated npm packages",
    );
    expect(source).toContain('<HealthSection id="outdated" count={outdatedCount}');
    expect(source).toContain("observedTotal(report, \"outdated npm packages\"");
  });

  /**
   * The exact retained/observed numbers are still rendered, by the summary's
   * caveat list rather than by a block in this file. `summary.test.ts` asserts
   * the wording against real notices ("cargo ecosystem artifacts: retained 24
   * of 36"), which is a stronger check than the template string match this
   * replaces: that one could not tell whether the numbers were right.
   */
  it("hands limit notices to the summary rather than re-rendering them", () => {
    expect(source).not.toContain("report.limit_notices");
    expect(source).toContain("summarizeHealth({");
  });
});

describe("HealthPanel flicker contracts", () => {
  it("tracks the scanned path with a plain object so the load effect cannot loop", () => {
    expect(source).toMatch(/const scanned = \{ path: "" \}/);
    expect(source).not.toMatch(/let scanned = \$state/);
  });

  it("hydrates the cached report before rescanning so revisits render instantly", () => {
    expect(source).toContain("createRepoPanelCache<{");
    expect(source).toContain("dependabotCheckedAt: number | null");
    expect(source).toContain("codeScanningCheckedAt: number | null");
    expect(source).toContain(
      "dependabotRequestFailed,",
    );
    const effectBody = source.slice(source.indexOf("scanned.path = path;"), source.indexOf("async function openExternal"));
    expect(effectBody).toContain("healthCache.get(path)");
    expect(effectBody).toContain("githubAlertsCache.get(path)");
    expect(effectBody).toContain("dependabotCheckedAt = cached.dependabotCheckedAt;");
    expect(effectBody).toContain("autoScanGithubAlerts");
    expect(effectBody).toContain("void scanDependabot(path)");
  });

  it("timestamps and caches the latest successful or failed GitHub check", () => {
    const body = source.slice(
      source.indexOf("async function scanDependabot"),
      source.indexOf("function renderedReport"),
    );
    expect(body).toContain("loadGithubAlerts(repoPath");
    expect(body).toContain("force: options?.force === true");
    expect(body).toContain("dependabotCheckedAt = snapshot.checkedAt");
    expect(body).toContain("codeScanningCheckedAt = snapshot.checkedAt");
    expect(body).toContain("cacheDependabotResult(");
    expect(body).toContain("snapshot.codeScanning");
  });

  it("does not resurrect an older GitHub result after the local rescan fails", () => {
    const helper = source.slice(
      source.indexOf("function cacheDependabotResult"),
      source.indexOf("async function scanDependabot"),
    );
    expect(helper).toContain(
      "const currentReport = scanned.path === repoPath ? report : null;",
    );
    expect(helper).toContain("currentReport ?? healthCache.get(repoPath)?.deps");
    expect(helper).toContain(
      "dependabotRequestFailed: requestFailed,",
    );
    expect(helper).toContain("codeScanning: codeScanningResult");
    expect(helper).toContain("codeScanningRequestFailed: codeScanningFailed");
  });

  it("renders an explicit age for cached Dependabot results", () => {
    const checkedAt = Date.UTC(2026, 8, 3, 12, 34, 56);
    const freshness = dependabotFreshness(checkedAt);
    expect(freshness.iso).toBe("2026-09-03T12:34:56.000Z");
    expect(freshness.label.length).toBeGreaterThan(0);
    expect(source).toContain("may be cached");
    expect(source).toContain("dependabotFreshness(dependabotCheckedAt)");
    // The machine timestamp is what a reader can act on across timezones, and
    // it is now on the always-visible summary line of the disclosure rather
    // than inside four lines of prose about GitHub CLI permissions.
    expect(source).toContain("<time datetime={displayedGithubFreshness.iso}>");
    const disclosure = source.slice(source.indexOf("<details"), source.indexOf("</summary>"));
    expect(disclosure, "freshness must stay visible when the note is collapsed").toContain(
      "displayedGithubFreshness",
    );
    expect(disclosure).toContain('role="status"');
  });

  it("never shows the previous repository's health data while an uncached repo scans", () => {
    const effectBody = source.slice(source.indexOf("scanned.path = path;"), source.indexOf("async function openExternal"));
    expect(effectBody).toContain("report = null;");
    expect(effectBody).toContain("dependabot = null;");
    expect(effectBody).toContain("dependabotCheckedAt = null;");
    expect(effectBody).toContain("codeScanning = null;");
    expect(effectBody).toContain("codeScanningCheckedAt = null;");
    expect(effectBody).toContain("deadSymbols = [];");
    expect(effectBody).toContain("codegraph = null;");
    expect(effectBody).toContain("dependabotInflight?.cancel();");
    expect(effectBody).toContain("checkingGithub = false;");
  });

  it("gates its loading placeholder on having no data yet", () => {
    expect(source).toContain("{#if loading && !report}");
  });
});

describe("HealthPanel error-state separation (regression)", () => {
  /**
   * `openExternal` failures are non-fatal: the advisory link did not open,
   * but the scan behind it is still valid. Routing them into the same state
   * that guards the "load failed" branch meant one failed link click
   * replaced the entire report with a bare banner — and made the in-report
   * banner (whose own comment promised otherwise) unreachable dead code.
   */
  it("does not let a non-fatal action error replace the whole report", () => {
    const opener = source.slice(
      source.indexOf("async function openExternal"),
      source.lastIndexOf("</script>"),
    );
    const assigned = opener.match(/(\w+)\s*=\s*reportPanelError\("health"/);
    expect(assigned, "openExternal must report its failure").not.toBeNull();
    const actionState = assigned![1];

    const chain = source.slice(source.indexOf("{#if loading && !report}"));
    const branches = [...chain.matchAll(/\{:else if ([^}]+)\}/g)].map((m) => m[1].trim());
    const reportBranch = branches.indexOf("report");
    expect(reportBranch, "the report branch must exist").toBeGreaterThanOrEqual(0);
    // Nothing that a non-fatal action sets may shadow the report branch.
    expect(branches.slice(0, reportBranch)).not.toContain(actionState);
  });

  it("keeps the action banner outside the load-error else-chain so it can render", () => {
    const chainStart = source.indexOf("{#if loading && !report}");
    expect(source.indexOf("{#if actionError}")).toBeGreaterThan(-1);
    // Rendered before the chain, like GitHubPanel's actionError banner, so it
    // is visible in every state rather than nested in one unreachable branch.
    expect(source.indexOf("{#if actionError}")).toBeLessThan(chainStart);
  });

  it("clears a stale action error when a new scan starts", () => {
    const scanBody = source.slice(
      source.indexOf("async function scan("),
      source.indexOf("function renderedReport"),
    );
    expect(scanBody).toContain("actionError = null");
  });

  it("tints the Dependabot header badge through the shared normalizer", () => {
    expect(source).toContain("dependabotBadgeClass");
    // Raw string equality on GitHub severities missed "HIGH"/"Critical",
    // which github/mod.rs passes through verbatim.
    expect(source).not.toMatch(/severity === "(critical|high|medium)"/);
  });

  it("does not treat a failed dead-code check as an empty clean graph", () => {
    expect(source).toContain("Dead-code check could not run");
    expect(source).toContain("No dead-code candidates in the indexed graph");
    expect(source).toContain("not stored — not that it is zero");
  });

  /**
   * This used to anchor on `">\n            Issues ("` — twelve literal
   * spaces of indentation — and on a slice running to the first `gp-segmented`
   * in the file. Both encoded where the markup happened to sit rather than
   * what it had to say, so reindenting the template or adding a segmented
   * control anywhere above broke a guard about counting.
   *
   * Counts are `SectionCount` objects now, rendered by one formatter that has
   * no branch printing a survivor count alone (`counts.test.ts` proves that
   * over its whole input space). So the guard here is the structural one: the
   * panel hands sections a count object, never a sentence it built itself.
   */
  it("hands every counted section a count object rather than a built string", () => {
    expect(source).toContain("observedTotal(");
    const counts = [...source.matchAll(/count=\{([^}]*)\}/g)].map((m) => m[1].trim());
    expect(counts.length, "no counted sections found").toBeGreaterThan(3);
    for (const expression of counts) {
      expect(
        expression,
        `a section builds its own count text: ${expression}`,
      ).not.toMatch(/["'`]/);
    }
    // Each count binding is built from an observed total, not a row count.
    expect(source).toContain("total: issuesTotal,");
    expect(source).toContain("total: outdatedTotal,");
    expect(source).toContain("total: vulnerabilitiesTotal }");
    expect(source).toContain("total: deadSymbolsTotal,");
  });

  /**
   * The "All" tab could always disclose a cap because `audit.total` is
   * computed before `cap_report` truncates. "Direct" has no equivalent total
   * — it filters the surviving rows — so it printed a floor as though it were
   * the count. The cap keeps the most severe findings, so the rows dropped
   * are exactly the ones nobody is looking at.
   */
  it("does not print the direct-vulnerability count as a total when the scan was capped", () => {
    expect(source).toContain("vulnerabilitiesCapped");
    const count = source.slice(
      source.indexOf("let vulnerabilityCount"),
      source.indexOf("let issuesCount"),
    );
    expect(count).toContain('filter === "direct"');
    expect(count).toContain("atLeast: vulnerabilitiesCapped,");
    expect(count).toContain('qualifier: "direct",');
    // The "at least" wording itself is the formatter's, and is asserted there.
    expect(count).not.toContain("at least");
  });

  /**
   * GitHub alerts can still be unchecked (preference off, or the fetch has
   * not returned). That state used to render as the same empty space as
   * "looked, nothing open". A clean local audit next to that blank read as an
   * all-clear for a repository whose alerts had never been fetched.
   */
  /**
   * The four GitHub states must stay distinguishable. They used to be four
   * literal strings in this header, crammed into a flex row of five
   * `truncate` spans that rendered as "· Dependabot 0 … · Code scanning
   * unav…" — distinguishable in the source and not on the screen.
   *
   * They are facets of the shared summary now. `summary.test.ts` asserts all
   * four for both feeds, including that no two share a value *or* a tone —
   * which the string matches never checked. What this file owns is that the
   * header stopped competing with them: only open alerts, the one state worth
   * header room, keep a badge here, and that badge does not clip.
   */
  it("keeps only open alert counts in the header, and does not truncate them", () => {
    const header = source.slice(
      source.indexOf('<div class="px-4 py-2 border-b'),
      source.indexOf('<div class="flex-1 overflow-auto'),
    );
    expect(header).toContain("{#if openDependabotCount > 0}");
    expect(header).toContain("{#if openCodeScanningCount > 0}");
    // The states that are not findings moved to the summary card, which wraps.
    expect(header).not.toContain("not checked");
    expect(header).not.toContain("unavailable");
    expect(header).not.toContain("0 open");
    // Nothing in the header's status row clips its own text any more.
    expect(header).not.toContain("truncate text-textMuted");
    expect(header).not.toContain("truncate text-amber-300");
  });

  it("renders the verdict from the summary owner in the header", () => {
    const header = source.slice(
      source.indexOf('<div class="px-4 py-2 border-b'),
      source.indexOf('<div class="flex-1 overflow-auto'),
    );
    expect(header).toContain("toneChipClass(summary.tone)");
    expect(header).toContain("{toneLabel(summary.tone)}");
    // The full sentence stays reachable without spending header width on a
    // string the summary card repeats verbatim a few rows below.
    expect(header).toContain("title={summary.headline}");
    expect(header).not.toContain(">{summary.headline}<");
  });

  /**
   * `cmd_codeintel_dead_symbols` answers under a token budget and returns
   * `total`/`truncated` next to the rows. Both were dropped on arrival, so
   * the heading counted surviving rows and called that the repository's
   * unreferenced-symbol count.
   */
  it("keeps the dead-symbol total and truncation flag rather than counting rows", () => {
    expect(source).toContain("deadSymbolsTotal");
    expect(source).toContain("deadSymbolsTruncated");
    const count = source.slice(
      source.indexOf("let deadCodeCount"),
      source.indexOf("let dependabotCount"),
    );
    expect(count).toContain("total: deadSymbolsTotal,");
    expect(count).toContain("atLeast: deadSymbolsTruncated,");
    // An empty result from a truncated query is not an all-clear.
    expect(source).toContain("this is not an all-clear");
  });

  it("carries incomplete call evidence through both the panel and copied report", () => {
    expect(source).toContain("dead.value.walk_incomplete");
    expect(source).toContain("walk_incomplete: deadSymbolsIncomplete");
    expect(source).toContain("Dead-code analysis is incomplete: ${deadSymbolsIncomplete}");
    expect(source).toContain("deadSymbols.length === 0 && !deadSymbolsIncomplete");
    expect(source).toContain("{formatDeadCodeStatus(sym)}");
    // The reason a table is unreliable now renders above it, not after it: a
    // reader who jumps to the section met the rows first and the caveat second.
    expect(source).toContain("caveat={deadCodeCaveat}");
    const section = source.slice(source.indexOf('id="dead-code"'));
    expect(section.indexOf("caveat={deadCodeCaveat}")).toBeLessThan(
      section.indexOf("<HealthTable"),
    );
  });

  /**
   * A scanner that was installed, dispatched and then failed left the summary
   * saying "incomplete" and naming nothing, because the panel derived only
   * the missing-CLI half of the reason.
   */
  /**
   * Two cuts hid behind one list: the backend keeps at most
   * MAX_ECOSYSTEM_MANIFESTS per family and records a notice, and the row then
   * printed only the first four of whatever survived — with nothing marking
   * either. "4 lockfiles" and "the 4 lockfiles there are" looked identical.
   */
  it("says how many ecosystem artifacts it is not showing", () => {
    expect(source).toContain("ecosystemArtifactTotal");
    expect(source).toContain("ecosystem artifacts`");
    expect(source).toContain("+{seen - shown.length} more");
    // The bare slice with no disclosure must be gone.
    expect(source).not.toContain('{eco.manifests.slice(0, 4).join(", ")}');
    // And the paths are a list, not a comma-joined run inside a paragraph:
    // four Rust crates used to render as three wrapped lines of file paths
    // with no structure to scan.
    expect(source).not.toContain('{shown.join(", ")}');
    expect(source).toContain("<dl");
    expect(source).toContain("<dt");
  });

  /**
   * The class behind every count defect this panel has had: a bounded
   * collection headlined by the rows that survived the bound.
   *
   * Derived from the source rather than from a list of today's sections, so a
   * new counted section is covered the day it is written. Every heading that
   * prints a count must either print an observed total (a `*Total` binding)
   * or print the count beside a truncation marker — `.length` on its own is
   * the shape that reads as complete coverage and is not.
   *
   * Caught in the wild by this rule: the dead-symbol heading counted the rows
   * that fitted the token budget, and the direct-vulnerability filter counted
   * the rows that survived the scan cap.
   */
  /**
   * The same class, now guarded structurally: the panel writes no `<h3>` of
   * its own at all, so there is no heading it can hand-build a count into.
   * Every section's heading comes from `HealthSection`, which renders counts
   * only through `formatSectionCount`.
   *
   * This is what the regex over heading text was reaching for and could not
   * get: it checked that a sentence mentioned a total, not that the number in
   * it was the observed one.
   */
  it("owns no headings of its own, so it cannot hand-build a count into one", () => {
    expect(source).not.toMatch(/<h3\b/);
    const sections = [...source.matchAll(/<HealthSection\b/g)];
    expect(sections.length, "sections are no longer rendered by the shared shell")
      .toBeGreaterThan(5);
  });

  it("renders its sections in the catalog's order", () => {
    // The rendered order and the summary card's jump order are one decision.
    // They were two, which is how the page came to open with an inventory of
    // package manifests above the repository's high-severity vulnerability.
    const rendered = [...source.matchAll(/<HealthSection[\s\S]{0,80}?id="([a-z-]+)"/g)]
      .map((m) => m[1])
      .concat([]);
    const fromGithub = source.indexOf("{#snippet githubSections()}");
    expect(fromGithub).toBeGreaterThan(-1);
    const catalogOrder = HEALTH_SECTIONS.map((s) => s.id);
    const seen = rendered.filter((id, index) => rendered.indexOf(id) === index);
    const positions = seen.map((id) => catalogOrder.indexOf(id));
    expect(positions.every((p) => p >= 0), `rendered ${seen.join(",")}`).toBe(true);
    expect(
      [...positions].sort((a, b) => a - b),
      "sections render out of catalog order",
    ).toEqual(positions);
  });

  it("names failed scanners, not only missing ones, when coverage is short", () => {
    expect(source).toContain("coverageGap");
    // The old wording derived only from `skippedAudits`; the single owner
    // now covers both halves, so the narrower one must be gone.
    expect(source).not.toContain("skippedAudits(report)");
    expect(source).toContain("Local audit incomplete${gap");
  });
});
