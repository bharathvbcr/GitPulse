# Generating coverage

The Coverage page reads local coverage artifacts and shows line hits against
source files. Select **Claude Code** or **Codex**, then use **Generate coverage**
(or **Improve coverage** for a measured report). GitPulse opens its terminal dock and creates a
new agent session in the selected checkout, supplying the previewed prompt as a
literal CLI argument. Existing sessions stay open. The agent keeps its configured
permissions; authentication, approval prompts and execution errors appear in the
terminal. Starting a session is not a successful coverage measurement.

**Copy agent prompt** copies the same task for an existing terminal session.
**Preview prompt** includes the checkout path, scanner-detected languages and
accepted artifact locations, plus the current coverage snapshot. It also works
before a successful scan. Failed scans, missing data, scan caps and recovered-run
exclusions remain explicit. Large prompts retain all task instructions and mark
clipped context; prompts are limited to 16,000 UTF-8 bytes.

The task asks the agent to inspect repository instructions, test configuration and
CI commands, run real tests, and export per-file, per-line records to paths the
scanner accepts. HTML reports, summary-only JSON, raw profiling data and Python's
binary `.coverage` database need an export step. GitPulse reads LCOV, Cobertura,
Go coverprofiles, Istanbul/NYC JSON, JaCoCo and Clover for their supported language
families. Prefer the paths shown for the specific repository; monorepo outputs
must preserve each suite's data and resolve source paths inside the checkout.

When the agent finishes, press **Rescan**. Confirm the expected artifacts and file
rows appear. A successful test command alone does not establish usable coverage.
Missing tools, failed tests and partial module coverage should remain visible in
the agent's result instead of being represented as a complete measurement.

**View agent session** returns to the exact launched tab, including its transcript
after the process exits. **Rescan results** reads artifacts without running tests.
The link survives coverage-page and repository switches, disappears when the tab
closes, and retains the 16 most recent agent tabs. Session states describe the
terminal process; an exited agent is not proof of generated coverage.

## Finding and improving gaps

Search paths without case sensitivity, combine language and missed-line filters,
and sort by missed-line count, lowest coverage or file path. The shown count is
relative to scanned files; scan-cap notices still disclose incomplete discovery.
Unmeasured files are excluded from missed-line filters. A selected file stays open
when filtered out, with a visible explanation and a **Clear filters** recovery.

**Previous / Next** moves between explicit zero-hit blocks and wraps at the ends.
**Alt+Up / Alt+Down** works while focus is inside Coverage, except in text-entry
controls. Unknown lines never count as misses. Navigation scrolls the virtual
source viewer and highlights the selected block; loading or unavailable details
disable navigation. Each new measurement refreshes line details even if its
aggregate percentage is unchanged.
Background scan updates stay subscribed during file selection and status polls;
unrelated store updates do not cancel pending source loads.

**Improve this file’s coverage** focuses the same preview/copy/run prompt on the
selected file, its measured totals and up to 100 available missed blocks. Missing
or partial line details remain explicit. Uncheck **Focus on …** to return to the
repository task. Changing selection updates the focused prompt.

The status strip distinguishes measured, unmeasured, partial and failed scans.
**Last scanned** is when GitPulse read the report. Artifact generation timestamps
and test outcomes are unavailable to the scanner, so it explicitly shows
**Artifact age unknown**. A failed rescan retains the previous snapshot with a
stale label and **Retry scan** action.

The launch syntax uses the interactive positional prompt supported by
[Codex](https://developers.openai.com/codex/cli/reference/) and
[Claude Code](https://code.claude.com/docs/en/cli-usage).

## Verification

Run `npm test -- src/lib/coverage src/lib/terminal src/lib/components/CoverageViewer.test.ts`
for formatter, exploration, handoff, tab and coverage regression tests, and `npm run check`
for Svelte/TypeScript checks. Run `npm run test:coverage-ui` for the automated
browser gate, also included in `ci:local`. Add `-- --webkit` to exercise macOS
WebKit. Failures and incomplete runs fail the gate.

Run `npm run dev`, open `/harness/coverage.html` on the reported local URL and click
**Run coverage checks**. The harness mounts the real Coverage page, dock, terminal
panels and xterm with Tauri's mock transport. It verifies copy/preview, missing and
failed scans, prompt refresh, both agents' exact program/argv/repository handoff,
session preservation, capacity refusal, failed spawning, filtering, missed-block
navigation, focused prompts, same-total line refreshes, terminal return links
and narrow layouts.
No agent commands execute in this harness; it does not prove an authenticated
agent run or native coverage ingestion. Reload the harness before rerunning.
