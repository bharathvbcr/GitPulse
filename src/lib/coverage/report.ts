import { formatCoveragePercent } from "./format";
import { coverageCommandsAreCumulative } from "./scripts";
import { observedTotal } from "../scan/limits";
import type {
  CoverageArtifact,
  CoverageFamilyStatus,
  CoverageLanguageSplit,
  CoverageReport,
  FileCoverageSummary,
} from "./types";

const MAX_FILES_LISTED = 30;
const MAX_ARTIFACTS_LISTED = 40;
const MAX_ISSUE_BODY_BYTES = 60 * 1024;
const ISSUE_CLIP_NOTE = "\n\n[GitPulse clipped this draft to stay below GitHub's body limit.]";

/**
 * Rendered instead of a percentage when no artifact contributed a single line
 * record. Distinguishing "not measured" from "measured 0%" is the whole point;
 * the parenthetical exists so a model or a reader skimming the line cannot
 * collapse them back together.
 */
export const NO_COVERAGE_DATA =
  "No coverage data — no artifact contributed line records (this is not 0% coverage)";

export interface CoverageIssueDraft {
  title: string;
  body: string;
  clipped: boolean;
}

/**
 * Coverage numbers cross the IPC boundary as plain JSON, so a buggy or
 * hostile producer can hand over NaN, Infinity, negatives or missing fields.
 * Everything is re-validated here: the renderer must hand the model (and the
 * clipboard) a well-formed document and must never throw on any input shape.
 */
function safePercent(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "0.0%";
  return formatCoveragePercent(Math.min(100, value));
}

function safeCount(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.trunc(value)));
}

function safeText(value: unknown): string {
  if (typeof value !== "string") return "";
  // Paths and producer-owned labels are rendered as one report line. Keep
  // embedded controls visible instead of letting a crafted filename inject a
  // new heading, runnable command, or terminal control into the model prompt.
  return Array.from(value, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (char === "\n") return "\\n";
    if (char === "\r") return "\\r";
    if (char === "\t") return "\\t";
    if ([0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069].includes(code)) {
      return `\\u{${code.toString(16)}}`;
    }
    if (code < 0x20 || code === 0x7f) return `\\u{${code.toString(16).padStart(4, "0")}}`;
    return char;
  }).join("");
}

function safeList<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

/**
 * Deterministic plain-text rendering of a coverage report, used both as the
 * payload for `cmd_ai_coverage_report` and for copy-to-clipboard. Section
 * order is stable so prompts and tests stay comparable across runs; capped
 * lists carry their counters so "shorter list" never reads as "everything".
 */
export function formatCoverageReport(report: CoverageReport, repoPath: string): string {
  const data = (report ?? {}) as Partial<CoverageReport>;
  const languageRows = safeList(data.languages);
  const fileRows = safeList(data.files);
  const artifactRows = safeList(data.artifacts);
  const familyRows = safeList(data.families);

  const out: string[] = [];
  out.push(`Coverage report — ${safeText(repoPath)}`);

  const linesFound = safeCount(data.overall?.lines_found);
  const linesHit = Math.min(safeCount(data.overall?.lines_hit), linesFound);
  // "0.0% (0/0 lines)" is what a repo with no parsable artifact produced and
  // what a repo whose every line is uncovered produced — the same sentence for
  // "we could not measure" and "we measured, it is bad". Only the second is a
  // coverage figure, and only the second should be actioned as one. A real 0%
  // still has lines_found > 0, so the two are separable here.
  let overall =
    linesFound === 0
      ? NO_COVERAGE_DATA
      : `${safePercent(data.overall?.percentage)} (${linesHit}/${linesFound} lines)`;
  // A capped scan must say so in the same breath as its totals.
  if (data.truncated === true) overall += " [SCAN TRUNCATED — results partial]";
  out.push("", "OVERALL", overall);

  if (languageRows.length > 0) {
    out.push("", "PER-LANGUAGE");
    for (const lang of languageRows as CoverageLanguageSplit[]) {
      if (!lang || typeof lang !== "object") continue;
      out.push(
        `${safeText(lang.language)}: ${safePercent(lang.percentage)} ` +
          `(${safeCount(lang.lines_hit)}/${safeCount(lang.lines_found)} lines, ${safeCount(lang.files)} files)`,
      );
    }
  }

  // report.files already arrives sorted worst-first; slicing preserves that.
  if (fileRows.length > 0) {
    // Two independent caps stack here: the scanner's `max_files` (disclosed by
    // a limit notice) and this renderer's MAX_FILES_LISTED. Naming only the
    // second turns "4,000 of 12,873 files were kept" into "141 files", which
    // reads as a complete inventory. Carry both numbers.
    const observedFiles = observedTotal(data, "covered files", fileRows.length);
    const scannerDropped =
      observedFiles > fileRows.length ? `; ${fileRows.length} retained by the scan cap` : "";
    out.push(
      "",
      `LOWEST-COVERED FILES (worst first, showing ${Math.min(fileRows.length, MAX_FILES_LISTED)} of ${observedFiles}${scannerDropped})`,
    );
    for (const file of fileRows.slice(0, MAX_FILES_LISTED) as FileCoverageSummary[]) {
      if (!file || typeof file !== "object") continue;
      out.push(
        `- ${safeText(file.path)}: ${safePercent(file.percentage)} ` +
          `(${safeCount(file.lines_hit)}/${safeCount(file.lines_found)} lines)`,
      );
    }
  }

  if (artifactRows.length > 0) {
    const observedArtifacts = observedTotal(data, "coverage artifacts", artifactRows.length);
    const artifactsDropped =
      observedArtifacts > artifactRows.length
        ? `; ${artifactRows.length} read before the scan cap`
        : "";
    out.push(
      "",
      `ARTIFACTS (showing ${Math.min(artifactRows.length, MAX_ARTIFACTS_LISTED)} of ${observedArtifacts}${artifactsDropped})`,
    );
    for (const artifact of artifactRows.slice(0, MAX_ARTIFACTS_LISTED) as CoverageArtifact[]) {
      if (!artifact || typeof artifact !== "object") continue;
      let line = `- ${safeText(artifact.path)} (${safeText(artifact.format)})`;
      if (artifact.skipped === true) {
        const reason = safeText(artifact.skip_reason);
        line += reason ? ` — skipped: ${reason}` : " — skipped";
      }
      out.push(line);
    }
    if (artifactRows.length > MAX_ARTIFACTS_LISTED) {
      out.push(`…and ${artifactRows.length - MAX_ARTIFACTS_LISTED} more`);
    }
  }

  const notices = safeList(data.limit_notices).filter(
    (notice) => !!notice && typeof notice === "object",
  );
  if (notices.length > 0) {
    out.push("", "SCAN LIMITS (the sections above are a bounded sample)");
    for (const notice of notices) {
      out.push(`- ${safeText(notice.resource)}: retained ${safeCount(notice.kept)} of ${safeCount(notice.total)}`);
    }
  }

  // Naming what was looked for but not found steers the model toward fixing
  // the missing data instead of inventing numbers for it.
  const missingFamilies = (
    familyRows as CoverageFamilyStatus[]
  ).filter((family) => !!family && typeof family === "object" && family.found !== true);
  if (missingFamilies.length > 0) {
    out.push("", "FAMILIES WITHOUT REPORT");
    for (const family of missingFamilies) {
      const formats = Array.isArray(family.expected_formats)
        ? family.expected_formats.map(safeText).filter(Boolean).join(", ")
        : "";
      const commands = Array.isArray(family.suggested_commands)
        ? family.suggested_commands.map(safeText).filter(Boolean)
        : [];
      const setup = Array.isArray(family.setup_commands)
        ? family.setup_commands.map(safeText).filter(Boolean)
        : [];
      const toolReady = family.tool_ready !== false;
      const toolDetail = safeText(family.tool_detail);
      const duration = safeText(family.duration_hint);
      const parts: string[] = [];
      if (!toolReady && toolDetail) parts.push(toolDetail);
      if (!toolReady && setup.length > 0) {
        parts.push(`setup ${setup.map((cmd) => `\`${cmd}\``).join(" then ")}`);
      }
      if (commands.length > 0) {
        const separator = coverageCommandsAreCumulative(family.family) ? " then " : " or ";
        parts.push(`run ${commands.map((cmd) => `\`${cmd}\``).join(separator)}`);
      }
      if (duration) parts.push(duration);
      if (parts.length === 0) {
        // A family listed with no reason and no command is a dead end: the
        // reader cannot tell whether GitPulse found no generator, failed to
        // probe one, or simply had nothing to say. The backend now always
        // sends a detail, so reaching this means the payload predates that or
        // was hand-built; state the uncertainty instead of printing a bare
        // label that reads as "nothing to do here".
        parts.push("no generator planned and no reason reported");
      }
      const extra = ` — ${parts.join(" — ")}`;
      out.push(
        `- ${safeText(family.family)}${
          formats ? ` (expected: ${formats})` : " (no artifact locations known)"
        }${extra}`,
      );
    }
  }

  return out.join("\n");
}

function sanitizeBodyText(value: unknown): string {
  if (typeof value !== "string") return "";
  return Array.from(value, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (char === "\n" || char === "\r" || char === "\t") return char;
    if (code < 0x20 || code === 0x7f) return `\\u{${code.toString(16).padStart(4, "0")}}`;
    return char;
  }).join("");
}

function indentBlock(value: string): string {
  return value
    .split("\n")
    .map((line) => `    ${line}`)
    .join("\n");
}

function clipUtf8(value: string, maxBytes: number): { text: string; clipped: boolean } {
  const encoder = new TextEncoder();
  if (encoder.encode(value).byteLength <= maxBytes) return { text: value, clipped: false };
  const noteBytes = encoder.encode(ISSUE_CLIP_NOTE).byteLength;
  const contentLimit = Math.max(0, maxBytes - noteBytes);
  let bytes = 0;
  let text = "";
  for (const char of value) {
    const width = encoder.encode(char).byteLength;
    if (bytes + width > contentLimit) break;
    text += char;
    bytes += width;
  }
  return { text: text + ISSUE_CLIP_NOTE, clipped: true };
}

/**
 * Builds the external GitHub issue payload for a coverage snapshot.
 *
 * The local absolute repository path is deliberately excluded (and redacted
 * from optional MANVI prose), command output is never copied, producer-owned
 * text is indented as literal data, and the UTF-8 body stays below the backend's
 * 64 KiB hard limit. Filing still goes through the existing guarded issue owner.
 */
export function buildCoverageIssueDraft(
  report: CoverageReport,
  repoPath: string,
  manviAnalysis?: string | null,
): CoverageIssueDraft {
  const data = (report ?? {}) as Partial<CoverageReport>;
  const partial = data.truncated === true ? " (partial scan)" : "";
  // Never file "address 0.0% line coverage" for a repo that produced no
  // measurement: that states a finding the scan did not make, and it is the
  // title a maintainer reads first.
  const title =
    safeCount(data.overall?.lines_found) === 0
      ? `test(coverage): no coverage data was produced${partial}`
      : `test(coverage): address ${safePercent(data.overall?.percentage)} line coverage${partial}`;

  // The first rendered line contains the local path and is useful only for the
  // local model/clipboard. Drop it before this content crosses to GitHub.
  const snapshot = formatCoverageReport(report, "")
    .split("\n")
    .slice(1)
    .join("\n")
    .trimStart();
  let analysis = sanitizeBodyText(manviAnalysis).trim();
  if (repoPath) analysis = analysis.split(repoPath).join("<repository>");

  const sections = [
    "<!-- gitpulse:coverage-report:v1 -->",
    "## Coverage snapshot",
    "",
    "Generated by GitPulse from bounded coverage artifacts. No local absolute path or command output is included.",
    "",
    indentBlock(snapshot || "No coverage data was available."),
  ];
  if (analysis) {
    sections.push("", "## MANVI analysis", "", indentBlock(analysis));
  }
  sections.push(
    "",
    "## Resolution checklist",
    "",
    "- [ ] Confirm the report is current and complete.",
    "- [ ] Add tests for the lowest-covered behavior, including failure paths.",
    "- [ ] Regenerate coverage and attach the verified before/after totals.",
  );

  const clipped = clipUtf8(sections.join("\n"), MAX_ISSUE_BODY_BYTES);
  return { title, body: clipped.text, clipped: clipped.clipped };
}

/**
 * A coverage command that did not deliver coverage.
 *
 * `status` distinguishes the two ways that happens, because they need
 * different fixes and must not be reported as one: `failed` is a non-zero
 * exit, while `no_data` is a command that exited 0 and still left the family
 * without a report. Defaults to `failed` for callers that predate the
 * distinction.
 */
export interface FailedCoverageScript {
  label: string;
  detail?: string | null;
  status?: "failed" | "no_data";
}

export interface FailedCoverageDiagnosticsOptions {
  repoPath?: string | null;
  scanError?: string | null;
}

const GO_MISSING_MODULE =
  /directory prefix\s+\S+\s+does not contain main module/i;

/**
 * pytest aborted during *collection*, not during a test.
 *
 * A module that runs `sys.exit()` at import time takes the whole session down
 * before anything is measured: pytest reports `INTERNALERROR` with a
 * `SystemExit` traceback and "no tests ran". It is a common shape in
 * repositories that keep runnable scripts next to their tests, because
 * pytest's default `python_files` patterns (`test_*.py` and `*_test.py`)
 * collect a script named e.g. `stress_test.py` and import it.
 *
 * Distinguishing this from an ordinary test failure matters: no test failed,
 * and no amount of fixing tests will help. The fix is to stop collecting that
 * file.
 */
const PYTEST_COLLECTION_ABORTED = /INTERNALERROR>[\s\S]*?\bSystemExit\b/i;

/** The `File "<path>", line N, in <module>` frame of a collection abort. */
const PYTEST_EXITING_MODULE = /File "([^"]+)", line (\d+), in <module>/g;

/**
 * The repository file whose import aborted collection — the last `<module>`
 * frame that is not inside a virtualenv or the interpreter's own stdlib.
 */
function pytestAbortingModule(output: string): string | null {
  let found: string | null = null;
  // `matchAll` rather than a bare `exec` loop: it works on a clone, so the
  // shared /g pattern's `lastIndex` is never carried between calls.
  for (const match of output.matchAll(PYTEST_EXITING_MODULE)) {
    const [, file, line] = match;
    if (!file || /[/\\](site-packages|dist-packages)[/\\]|<frozen/.test(file)) continue;
    found = `${file}:${line}`;
  }
  return found;
}

const TEST_SUITE_RAN_FAILURE = [
  /Test Files\s+\d+\s+failed/i,
  /Failed Tests/i,
  /\bFAIL\s+\S+\.(t|j)sx?\b/i,
  /Tests:\s+\d+\s+failed/i,
  /not wrapped in act\s*\(/i,
  /React does not recognize the `[A-Za-z0-9$_]+` prop on a DOM element/i,
  /^FAILED\s+\S+/m,
  /^--- FAIL:/m,
];

/**
 * Classifies a failed coverage command so copied diagnostics name the real
 * problem: a missing Go module root, or a generator that ran and whose tests
 * failed — never "try a different ecosystem command" for those cases.
 */
export function coverageFailureHint(
  command: unknown,
  detail: unknown,
): string | null {
  const output = typeof detail === "string" ? detail : "";
  if (!output.trim()) return null;
  if (GO_MISSING_MODULE.test(output)) {
    return "Go was run from a directory without go.mod. Generate coverage from the module root with `go -C <module-dir> test ./... -coverprofile=coverage.out`.";
  }
  if (
    /No such file or directory \(os error 2\)/i.test(output) ||
    /Failed to spawn \S+/i.test(output)
  ) {
    return "That generator is not installed. GitPulse will not plan it unless the binary is on PATH.";
  }
  if (/outside the purpose-specific command allowlist/i.test(output)) {
    return "That command is not on the coverage allowlist. The scanner should not offer it.";
  }
  if (
    /npx canceled due to missing packages/i.test(output) ||
    /no YES option/i.test(output)
  ) {
    return "npx --no-install will not download a missing runner. Declare a coverage script or install the package locally.";
  }
  if (
    /Cannot find dependency '@vitest\/coverage-v8'/i.test(output) ||
    /MISSING DEPENDENCY/i.test(output)
  ) {
    return "Vitest ran without a coverage provider. Add @vitest/coverage-v8 or use the package.json coverage script.";
  }
  if (/wrapper ['`].*['`] is not a repository file/i.test(output)) {
    return "No Gradle wrapper in this repository. GitPulse will not invent ./gradlew.";
  }
  if (PYTEST_COLLECTION_ABORTED.test(output)) {
    const origin = pytestAbortingModule(output);
    const where = origin ? ` The module was ${origin}.` : "";
    return `pytest never ran a test: importing a collected module called sys.exit(), which aborts the whole session.${where} That file matches pytest's default collection patterns (test_*.py, *_test.py) but is a runnable script, not a test module. Rename it, guard its body with \`if __name__ == "__main__":\`, or exclude it (\`--ignore=<path>\`, or \`norecursedirs\`/\`python_files\` in pytest.ini).`;
  }
  if (TEST_SUITE_RAN_FAILURE.some((pattern) => pattern.test(output))) {
    const cmd = typeof command === "string" ? command : "";
    const runner = /\bvitest\b|\bjest\b|npm run/.test(cmd)
      ? "The coverage generator ran; tests failed."
      : "Tests ran and failed.";
    return `${runner} This is a test failure, not the wrong ecosystem command.`;
  }
  return null;
}

function statusLine(status: FailedCoverageScript["status"]): string {
  return status === "no_data"
    ? "Status: exited 0 but produced no coverage data"
    : "Status: failed";
}

function appendFailureBlock(
  out: string[],
  label: string,
  detail: string | null | undefined,
  status: FailedCoverageScript["status"],
): void {
  out.push(`Command: ${label}`);
  out.push(statusLine(status));
  const hint = coverageFailureHint(label, detail);
  if (hint) out.push(`Hint: ${hint}`);
  out.push("Output:");
  out.push(detail ? detail.trim() : "(no output recorded)");
}

/**
 * Formats failed coverage commands, generator scripts, and scan errors into a
 * clean plain-text diagnostic report suitable for copying to clipboard.
 */
export function formatFailedCoverageDiagnostics(
  failures: readonly FailedCoverageScript[],
  options?: FailedCoverageDiagnosticsOptions,
): string {
  const repo = options?.repoPath ? safeText(options.repoPath) : "";
  const scanErr = options?.scanError ? options.scanError.trim() : "";
  const validFailures = (failures ?? []).filter(
    (f): f is FailedCoverageScript => Boolean(f && typeof f === "object" && typeof f.label === "string"),
  );

  if (validFailures.length === 0 && !scanErr) {
    return "No coverage failures recorded.";
  }

  const out: string[] = [];
  out.push(repo ? `Coverage failure diagnostics — ${repo}` : "Coverage failure diagnostics");

  if (scanErr) {
    out.push(`Scan error: ${scanErr}`);
  }

  if (validFailures.length === 1) {
    const f = validFailures[0];
    if (scanErr) out.push("");
    appendFailureBlock(out, f.label, f.detail, f.status);
  } else if (validFailures.length > 1) {
    if (scanErr) out.push("");
    out.push(`Unsuccessful coverage commands (${validFailures.length}):`);
    validFailures.forEach((f, idx) => {
      out.push("");
      out.push(`[${idx + 1}] Command: ${f.label}`);
      out.push(statusLine(f.status));
      const hint = coverageFailureHint(f.label, f.detail);
      if (hint) out.push(`Hint: ${hint}`);
      out.push("Output:");
      out.push(f.detail ? f.detail.trim() : "(no output recorded)");
    });
  }

  return out.join("\n");
}

